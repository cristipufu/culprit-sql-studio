//! Real completed spill counters, qualified only inside an owned loopback database.
use super::*;
use crate::{
    conn::{MsClient, Pool, SessionKind},
    read::{Parameter, ReadError, ReadSession},
    test_support::{batch, config},
};
use futures_util::FutureExt;
use std::{
    panic::{AssertUnwindSafe, resume_unwind},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const COMMAND_LIMIT: Duration = Duration::from_secs(10);
const WORKLOAD_CALLS: usize = 2;
const CACHE_ROWS: usize = 128;
const FIXTURE_ROWS: i64 = 8192;
const PAYLOAD_BYTES: i64 = 512;

// Fixed 4 MiB input. Independent hashes avoid a repeated-character payload whose compressibility hides spills.
const SETUP: &str = r#"SET NOCOUNT ON;
CREATE TABLE dbo.cached_spill_fixture(id int NOT NULL PRIMARY KEY,payload char(512) NOT NULL);
WITH d AS(SELECT v FROM(VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9),(10),(11),(12),(13),(14),(15))x(v)),
n AS(SELECT 1+a.v+16*b.v+256*c.v+4096*e.v AS id FROM d a CROSS JOIN d b CROSS JOIN d c CROSS JOIN(VALUES(0),(1))e(v))
INSERT dbo.cached_spill_fixture SELECT id,
CONVERT(varchar(64),HASHBYTES('SHA2_256',CONCAT(id,':0')),2)+
CONVERT(varchar(64),HASHBYTES('SHA2_256',CONCAT(id,':1')),2)+
CONVERT(varchar(64),HASHBYTES('SHA2_256',CONCAT(id,':2')),2)+
CONVERT(varchar(64),HASHBYTES('SHA2_256',CONCAT(id,':3')),2)+
CONVERT(varchar(64),HASHBYTES('SHA2_256',CONCAT(id,':4')),2)+
CONVERT(varchar(64),HASHBYTES('SHA2_256',CONCAT(id,':5')),2)+
CONVERT(varchar(64),HASHBYTES('SHA2_256',CONCAT(id,':6')),2)+
CONVERT(varchar(64),HASHBYTES('SHA2_256',CONCAT(id,':7')),2) FROM n"#;

// The constrained grant is fixture input, never proposed application or collection SQL. Only one scalar returns.
const PROCEDURE: &str = r#"CREATE PROCEDURE dbo.cached_spill_query AS
SELECT SUM(CONVERT(bigint,rn)*id) AS cached_spill_positive_canary
FROM (SELECT id,ROW_NUMBER() OVER(ORDER BY payload,id) AS rn FROM dbo.cached_spill_fixture) ordered
OPTION(MAXDOP 1,MAX_GRANT_PERCENT=0.01)"#;

async fn fixture_batch(client: &mut MsClient, sql: &str) {
    tokio::time::timeout(COMMAND_LIMIT, batch(client, sql)).await.expect("bounded owned fixture command");
}

async fn execute_scalar(client: &mut MsClient) -> i64 {
    let results = tokio::time::timeout(COMMAND_LIMIT, async {
        client.simple_query("EXEC dbo.cached_spill_query").await.unwrap().into_results().await.unwrap()
    })
    .await
    .expect("bounded owned spill execution");
    let [rows] = results.as_slice() else { panic!("spill fixture must return exactly one result set") };
    let [row] = rows.as_slice() else { panic!("spill fixture must return exactly one scalar row") };
    assert_eq!(row.columns().len(), 1);
    row.get::<i64, _>(0).expect("the fixture aggregate is non-null bigint")
}

fn observer_sql() -> String {
    format!(
        "SELECT TOP(2) {context},qs.execution_count,qs.total_spills,qs.last_spills,qs.min_spills,qs.max_spills,qs.total_worker_time,qs.total_elapsed_time,qs.total_logical_reads {relation} WHERE {database} AND st.objectid=OBJECT_ID(N'dbo.cached_spill_query') OPTION(MAXDOP 1)",
        context = cached_statements::context_columns(),
        relation = cached_statements::relation(),
        database = cached_statements::DATABASE_PREDICATE
    )
}

fn native_spills(row: &Rec) -> CachedCounterDistribution {
    let exact = |name| integer(row, name).unwrap().to_string();
    CachedCounterDistribution {
        total: exact("total_spills"),
        last: exact("last_spills"),
        minimum: exact("min_spills"),
        maximum: exact("max_spills"),
    }
}

async fn prove_positive_spills(pool: Arc<Pool>) {
    let mut actor = pool.connect(SessionKind::User).await.unwrap();
    let setup_started = Instant::now();
    fixture_batch(&mut actor, SETUP).await;
    fixture_batch(&mut actor, PROCEDURE).await;
    let shape = tokio::time::timeout(COMMAND_LIMIT, crate::conn::query_one(
        &mut actor,
        "SELECT COUNT_BIG(*) AS fixture_rows,SUM(CONVERT(bigint,DATALENGTH(payload))) AS payload_bytes FROM dbo.cached_spill_fixture",
    ))
    .await
    .expect("bounded fixture shape check")
    .unwrap();
    assert_eq!(integer(&shape, "fixture_rows").unwrap(), FIXTURE_ROWS);
    assert_eq!(integer(&shape, "payload_bytes").unwrap(), FIXTURE_ROWS * PAYLOAD_BYTES);
    let setup_ms = setup_started.elapsed().as_secs_f64() * 1000.0;
    let workload_started = Instant::now();
    let first = execute_scalar(&mut actor).await;
    assert!(first > 0);
    for _ in 1..WORKLOAD_CALLS {
        assert_eq!(execute_scalar(&mut actor).await, first);
    }
    let workload_ms = workload_started.elapsed().as_secs_f64() * 1000.0;
    drop(actor);

    let mut reader = ReadSession::new(
        pool,
        CollectionBudget {
            page_rows: CACHE_ROWS as u32,
            max_snapshot_rows: CACHE_ROWS as u32,
            page_bytes: 1024 * 1024,
            total_bytes: 4 * 1024 * 1024,
            max_queries: 4,
            statement_timeout_ms: 3000,
            total_timeout_ms: 20_000,
            ..Default::default()
        },
    );
    let readback: Result<_> = async {
        let layout = Layout::read(&mut reader).await?;
        let before = reader.query(&observer_sql(), vec![], 2).await.map_err(ReadError::into_error)?;
        let rows = reader
            .query(&select_sql(&layout), vec![Parameter::Integer(CACHE_ROWS as i64 + 1)], CACHE_ROWS + 1)
            .await
            .map_err(ReadError::into_error)?;
        let after = reader.query(&observer_sql(), vec![], 2).await.map_err(ReadError::into_error)?;
        Ok((layout, before, rows, after))
    }
    .await;
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let metrics = reader.read_metrics().unwrap();
    assert!(!metrics.has_unconfirmed_work());
    assert_eq!(reader.queries(), 4);
    let (layout, before, rows, after) = readback.unwrap();
    assert!(layout.supports(CachedRuntimeMetric::SpilledPages), "native fixture needs the optional spill columns");
    assert!(rows.len() <= CACHE_ROWS);
    let [before] = before.as_slice() else { panic!("one native procedure statement must be cached") };
    let [after] = after.as_slice() else { panic!("the exact procedure statement must remain cached") };
    let context = cached_statements::context(before).unwrap();
    assert_eq!(cached_statements::context(after).unwrap(), context);
    let expected = native_spills(before);
    assert_eq!(native_spills(after), expected, "observation cannot change the selected execution counters");
    assert_eq!(integer(before, "execution_count").unwrap(), WORKLOAD_CALLS as i64);
    assert_eq!(integer(after, "execution_count").unwrap(), WORKLOAD_CALLS as i64);
    assert!(expected.total.parse::<u64>().unwrap() > 0, "the owned workload must establish positive native spills");
    assert!(expected.maximum.parse::<u64>().unwrap() > 0);
    let runtime = rows.iter().map(decode).collect::<Result<Vec<_>>>().unwrap();
    let selected = runtime.iter().filter(|row| row.context == context).collect::<Vec<_>>();
    let [selected] = selected.as_slice() else { panic!("producer must preserve exactly one identical native context") };
    assert_eq!(selected.counters.get(&CachedRuntimeMetric::SpilledPages), Some(&expected));
    let executions = WORKLOAD_CALLS.to_string();
    assert_eq!(selected.executions.as_deref(), Some(executions.as_str()));
    assert!(selected.is_valid() && selected.measured());
    println!(
        "{}",
        serde_json::json!({"fixture_rows":FIXTURE_ROWS,"payload_bytes":FIXTURE_ROWS*PAYLOAD_BYTES,
        "calls":WORKLOAD_CALLS,"setup_client_ms":setup_ms,"workload_client_ms":workload_ms,
        "native_worker_us":integer(before,"total_worker_time").unwrap(),
        "native_elapsed_us":integer(before,"total_elapsed_time").unwrap(),
        "native_logical_reads":integer(before,"total_logical_reads").unwrap(),"spill_pages":expected,
        "observer_metrics":metrics})
    );
}

#[tokio::test]
#[ignore = "requires owned loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn native_cached_spill_pages_preserve_all_four_counters_and_exact_context() {
    let port = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PORT").unwrap().parse::<u16>().unwrap();
    let password = std::env::var("CULPRIT_MSSQL_SOURCE_TEST_PASSWORD").unwrap();
    let master = Pool::new(config(port, "master", &password, "spill-fixture-admin"), None);
    let mut admin = master.connect(SessionKind::User).await.unwrap();
    let database =
        format!("culprit_cached_spill_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis());
    fixture_batch(&mut admin, &format!("CREATE DATABASE [{database}]")).await;
    let pool = Arc::new(Pool::new(config(port, &database, &password, "spill-fixture"), None));
    let result = AssertUnwindSafe(prove_positive_spills(pool.clone())).catch_unwind().await;
    drop(pool);
    fixture_batch(
        &mut admin,
        &format!("ALTER DATABASE [{database}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;DROP DATABASE [{database}]"),
    )
    .await;
    if let Err(panic) = result {
        resume_unwind(panic);
    }
}
