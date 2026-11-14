//! App writes run only in a nonce database on the disposable loopback fixture. The observer reads metadata.
use super::*;
use crate::{
    activity::{IdentityDetail, ReadOptions},
    conn::{Pool, SessionKind, map_error},
    read::{ReadResult, ReadSession},
    test_support::{assert_no_execution_grant, batch, config, session_id},
};
use futures_util::FutureExt;
use std::{panic::AssertUnwindSafe, sync::Arc, time::Duration};

const WORKLOAD: &str = r#"
/* culprit fixture: retained task counters are not attributed to the later WAITFOR */
CREATE TABLE #culprit_task_space_fixture(payload varchar(2000) NOT NULL);
WITH digits AS(SELECT v FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9))d(v))
INSERT #culprit_task_space_fixture
SELECT REPLICATE('x',2000) FROM digits a CROSS JOIN digits b CROSS JOIN digits c OPTION(MAXDOP 1);
WAITFOR DELAY '00:00:20';
DROP TABLE #culprit_task_space_fixture;
"#;

struct RecordedCall {
    sql: String,
    parameters: Vec<Parameter>,
    row_limit: usize,
}
struct RecordedRead {
    owner: ReadSession,
    calls: Vec<RecordedCall>,
}
fn copy_parameters(parameters: &[Parameter]) -> Vec<Parameter> {
    parameters
        .iter()
        .map(|value| match value {
            Parameter::Integer(value) => Parameter::Integer(*value),
            Parameter::Text(value) => Parameter::Text(value.clone()),
        })
        .collect()
}
#[async_trait::async_trait]
impl NativeRead for RecordedRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>> {
        self.calls.push(RecordedCall { sql: sql.into(), parameters: copy_parameters(&parameters), row_limit });
        self.owner.query(sql, parameters, row_limit).await
    }
    fn queries(&self) -> u32 {
        self.owner.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.owner.read_metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.owner.close().await
    }
}

async fn wait_for_fixture_request(pool: Arc<Pool>, session: &str) {
    let mut reader = ReadSession::new(
        pool,
        CollectionBudget { max_queries: 40, statement_timeout_ms: 1000, total_timeout_ms: 3000, ..Default::default() },
    );
    let result = AssertUnwindSafe(async {
        for _ in 0..30 {
            let rows = reader.query(
                "SELECT TOP(1) /* culprit fixture */ command FROM sys.dm_exec_requests WHERE session_id=@P1 OPTION(MAXDOP 1)",
                vec![Parameter::Integer(session.parse().unwrap())],
                1,
            ).await.unwrap();
            if rows.first().is_some_and(|row| row.string("command") == "WAITFOR") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the bounded owned task did not reach its post-allocation WAITFOR");
    })
    .catch_unwind()
    .await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn compare_native_task(pool: Arc<Pool>, observed: &TaskSpaceObservation) {
    let mut reader = ReadSession::new(
        pool,
        CollectionBudget { max_queries: 1, statement_timeout_ms: 1000, total_timeout_ms: 1000, ..Default::default() },
    );
    let native = reader.query(
        "SELECT TOP(1) /* culprit fixture */ user_objects_alloc_page_count,user_objects_dealloc_page_count,internal_objects_alloc_page_count,internal_objects_dealloc_page_count FROM sys.dm_db_task_space_usage WHERE session_id=@P1 AND request_id=@P2 AND exec_context_id=@P3 OPTION(MAXDOP 1)",
        vec![Parameter::Integer(observed.task.session_id.parse().unwrap()),Parameter::Integer(observed.task.request_id.parse().unwrap()),Parameter::Integer(observed.task.execution_context_id.parse().unwrap())],
        1,
    ).await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let native = native.unwrap();
    let [native] = native.as_slice() else { panic!("the owned native task disappeared before comparison") };
    for (field, observed) in [
        ("user_objects_alloc_page_count", &observed.user_allocated_pages),
        ("user_objects_dealloc_page_count", &observed.user_deallocated_pages),
        ("internal_objects_alloc_page_count", &observed.internal_allocated_pages),
        ("internal_objects_dealloc_page_count", &observed.internal_deallocated_pages),
    ] {
        assert_eq!(&optional_counter(native, field).unwrap(), observed, "native {field}");
    }
}

fn record_plan_work(query: usize, plan: &str) {
    // Preserve native per-operator measurements; their absence is unknown, and parent/child reads are not summed.
    culprit_core::xml::read(plan, |document| {
        let document = document.unwrap();
        for node in document.descendants().filter(|node| node.is_element()) {
            if matches!(node.tag_name().name(), "RunTimeCountersPerThread" | "QueryTimeStats") {
                let counters = node
                    .attributes()
                    .map(|attribute| (attribute.name(), attribute.value()))
                    .collect::<BTreeMap<_, _>>();
                let operator = node.ancestors().find(|node| node.has_tag_name("RelOp"));
                eprintln!(
                    "owned task metadata work: query={query},node={:?},operator={:?},native={}",
                    operator.and_then(|node| node.attribute("NodeId")),
                    operator.and_then(|node| node.attribute("PhysicalOp")),
                    serde_json::to_string(&counters).unwrap()
                );
            }
        }
    });
}

async fn prove_task_space(pool: Arc<Pool>, session: &str, port: u16) {
    wait_for_fixture_request(pool.clone(), session).await;
    let budget = crate::task_space::bounded_budget(&CollectionBudget::default());
    assert_eq!((budget.max_queries, budget.statement_timeout_ms, budget.total_timeout_ms), (7, 1000, 5000));
    let mut reader = ActivityReader::new(
        RecordedRead { owner: ReadSession::new(pool.clone(), budget.clone()), calls: vec![] },
        ReadOptions { budget, scope: ActivityScope::CurrentDatabase, identity: IdentityDetail::Endpoint },
        "127.0.0.1".into(),
        port,
    );
    let captured = reader.capture_task_space().await;
    let verified = match &captured {
        Ok(captured) => reader.verify_task_space_context(&captured.context).await,
        Err(_) => Ok(false),
    };
    let shutdown = reader.close().await;
    assert_eq!(shutdown, SourceShutdown::Quiescent, "uncertain native read cannot publish task evidence");
    let captured = captured.unwrap();
    assert!(verified.unwrap());
    assert!(!captured.limited);
    assert!(captured.snapshot.is_valid());
    let task = captured
        .snapshot
        .observations
        .iter()
        .find(|row| row.task.session_id == session && row.task.execution_context_id == "0")
        .expect("the owned request has a native coordinator task");
    assert!(task.user_allocated_pages.as_deref().unwrap().parse::<u64>().unwrap() > 0);
    assert_eq!(task.bytes_per_page, 8192);
    assert_eq!(task.request_context(), TaskRequestContext::CheckedAtBounds);
    assert!(task.before_check.as_ref().unwrap().observed_at >= task.observed_at);
    assert!(task.after_check.as_ref().unwrap().observed_at >= task.before_check.as_ref().unwrap().observed_at);
    let request = task.request.as_ref().unwrap();
    let DefinitionPayload::Available { text, .. } = &request.statement else {
        panic!("the checked current WAITFOR statement was not available")
    };
    assert!(text.contains("WAITFOR") && !text.contains("INSERT"), "only current context is associated: {text}");
    assert!(!request.key.request.as_ref().unwrap().generation.ends_with('Z'));
    compare_native_task(pool.clone(), task).await;
    assert_eq!(reader.query_count(), 7, "six source reads plus the collector's final identity verification");
    assert_eq!(reader.io.calls[1].row_limit, MAX_NATIVE_ROWS + 1);
    assert!(reader.io.calls.iter().all(|call| call.sql.contains("LOOP JOIN") && call.row_limit <= MAX_NATIVE_ROWS + 1));
    let metrics = reader.read_metrics().unwrap();
    assert!(metrics.is_valid());
    assert!(metrics.templates.iter().all(|row| row.timeouts == 0 && row.unconfirmed_cancellations == 0));
    eprintln!("owned task metadata acquisition: {}", serde_json::to_string(&metrics).unwrap());

    let mut qualification = pool.connect(SessionKind::User).await.unwrap();
    for (query, call) in reader.io.calls.iter().enumerate() {
        for plan in assert_no_execution_grant(&mut qualification, &call.sql, copy_parameters(&call.parameters)).await {
            record_plan_work(query, &plan);
        }
    }
    // This small owned fixture qualifies the observed plan, not high-volume native DMV traversal under production load.
}

#[tokio::test]
#[ignore = "requires a disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_task_allocations_keep_checked_current_context_without_allocator_or_session_total_inference() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let database = format!("culprit_task_space_{}_{}", std::process::id(), chrono::Utc::now().timestamp_micros());
    let mut admin =
        crate::test_support::connect(&config(port, "master", &password, "task-fixture-admin"), SessionKind::User)
            .await
            .unwrap();
    let mut actor = None;
    let result = AssertUnwindSafe(async {
        batch(&mut admin, &format!("/* culprit fixture */ CREATE DATABASE [{database}]")).await;
        let pool = Arc::new(Pool::new(config(port, &database, &password, "culprit"), None));
        let mut client =
            crate::test_support::connect(&config(port, &database, &password, "task-space-fixture"), SessionKind::User)
                .await
                .unwrap();
        let session = session_id(&mut client).await;
        actor = Some(tokio::spawn(async move {
            client.simple_query(WORKLOAD).await.map_err(map_error)?.into_results().await.map(|_| ()).map_err(map_error)
        }));
        prove_task_space(pool, &session, port).await;
        assert!(!actor.as_ref().unwrap().is_finished(), "the stable fixture task ended during native qualification");
    })
    .catch_unwind()
    .await;
    // Only the nonce database is rolled back. Await every actor even when qualification panics or a read fails.
    let cleanup = AssertUnwindSafe(batch(&mut admin,&format!("/* culprit fixture */ IF DB_ID(N'{database}') IS NOT NULL BEGIN ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}];END"))).catch_unwind().await;
    if let Some(mut actor) = actor
        && tokio::time::timeout(Duration::from_secs(5), &mut actor).await.is_err()
    {
        actor.abort();
        let _ = actor.await;
        panic!("owned task actor did not finish after nonce database removal");
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    cleanup.expect("owned nonce database removed");
}
