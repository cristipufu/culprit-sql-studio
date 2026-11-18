//! These are labelled reader-protocol experiments, never substituted for task-space template measurements.
use super::*;

const CANCEL_SQL: &str = "WAITFOR DELAY '00:00:05'; SELECT /* culprit fixture cancellation */ 1 AS completed";
const STREAM_SQL: &str = "SELECT TOP(4096) /* culprit fixture backpressure */ REPLICATE(CONVERT(varchar(max),'x'),1024) AS payload FROM sys.all_objects a CROSS JOIN sys.all_objects b; WAITFOR DELAY '00:00:05'";

pub(crate) async fn lifecycle_cases(
    pool: Arc<Pool>,
    port: u16,
    password: &str,
    database: &str,
) -> culprit_core::Result<Vec<Value>> {
    let mut cases = Vec::new();
    cases.push(retired_refuses_rearm(pool.clone(), port, password, database).await?);
    cases.push(cancelled(pool.clone(), port, password, database).await?);
    if stop_on_uncertainty(&mut cases) {
        return Ok(cases);
    }
    cases.push(timed_out(pool.clone(), port, password, database).await?);
    if stop_on_uncertainty(&mut cases) {
        return Ok(cases);
    }
    cases.push(backpressured(pool.clone(), port, password, database).await?);
    if stop_on_uncertainty(&mut cases) {
        return Ok(cases);
    }
    cases.push(uncertain_refuses_rearm(pool, port, password, database).await?);
    Ok(cases)
}

fn stop_on_uncertainty(cases: &mut Vec<Value>) -> bool {
    if cases.last().is_some_and(|case| case["shutdown"] == json!(shutdown_state(SourceShutdown::Unconfirmed))) {
        cases.push(json!({"case":"remaining-lifecycle-cases","state":"not-run","reason":"original source pool remains fenced after unconfirmed settlement; no reconnect or replacement source work"}));
        true
    } else {
        false
    }
}

async fn cancelled(pool: Arc<Pool>, port: u16, password: &str, database: &str) -> culprit_core::Result<Value> {
    let budget = CollectionBudget { statement_timeout_ms: 1500, total_timeout_ms: 3000, ..Default::default() };
    let mut reader = crate::test_support::measured_read(pool, port, password, database, budget.clone()).await?;
    let seen = reader.active_seen();
    {
        let work = reader.query(CANCEL_SQL, vec![], 1);
        tokio::pin!(work);
        let deadline = tokio::time::sleep(Duration::from_secs(1));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                result=&mut work => return Err(result.err().map(ReadError::into_error).unwrap_or_else(invalid_data)),
                _=&mut deadline => return Err(invalid_data()),
                _=tokio::time::sleep(Duration::from_millis(10)) => if seen.load(Ordering::Acquire) { break; },
            }
        }
    }
    let before = reader.queries();
    let before_deadline = reader.owner.deadline;
    let before_metrics = reader.owner.read_metrics();
    assert!(reader.owner.measurement_window(budget).is_err(), "active native work cannot receive a new budget");
    assert_eq!(reader.queries(), before);
    assert_eq!(reader.owner.deadline, before_deadline);
    assert_eq!(reader.owner.read_metrics(), before_metrics);
    let shutdown = reader.close().await;
    let quiet = reader.observer.is_quiet().await?;
    assert!(quiet, "the exact original native request survived close");
    // A quiet observer never overwrites a missing driver acknowledgement.
    if shutdown == SourceShutdown::Unconfirmed {
        assert_no_followup(&mut reader).await;
    }
    Ok(json!({
        "case":"caller-cancellation",
        "source_template":false,
        "active_native_request_observed":true,
        "shutdown":shutdown_state(shutdown),
        "independently_observed_quiet":quiet,
        "accounting":reader.report(),
    }))
}

async fn timed_out(pool: Arc<Pool>, port: u16, password: &str, database: &str) -> culprit_core::Result<Value> {
    let budget = CollectionBudget { statement_timeout_ms: 100, total_timeout_ms: 1000, ..Default::default() };
    let mut reader = crate::test_support::measured_read(pool, port, password, database, budget).await?;
    let result = reader.query(CANCEL_SQL, vec![], 1).await;
    let shutdown = reader.close().await;
    assert!(
        matches!(&result, Err(ReadError::Stopped(CollectionStop::TimedOut | CollectionStop::CancellationUnconfirmed))),
        "the deadline experiment must retain a typed native-reader stop: {result:?}"
    );
    let active_observed = reader.active_seen().load(Ordering::Acquire);
    let template_admitted = reader.read_metrics().is_some_and(|metrics| {
        metrics.templates.iter().any(|phase| phase.template_id == digest(CANCEL_SQL.as_bytes()))
    });
    let quiet = reader.observer.is_quiet().await?;
    assert!(quiet);
    if shutdown == SourceShutdown::Unconfirmed {
        assert_no_followup(&mut reader).await;
    }
    Ok(json!({
        "case":if active_observed {"statement-timeout"}else{"deadline-with-unverified-native-start"},
        "native_template_admitted":template_admitted,
        "active_native_request_observed":active_observed,
        "source_template":false,
        "error":result.err().map(ReadError::into_error).map(|error|error.code),
        "shutdown":shutdown_state(shutdown),
        "independently_observed_quiet":quiet,
        "accounting":reader.report(),
    }))
}

async fn backpressured(pool: Arc<Pool>, port: u16, password: &str, database: &str) -> culprit_core::Result<Value> {
    let budget = CollectionBudget {
        page_rows: 1,
        page_bytes: 2048,
        statement_timeout_ms: 300,
        total_timeout_ms: 1500,
        ..Default::default()
    };
    let mut reader = crate::test_support::measured_read(pool, port, password, database, budget).await?;
    reader.begin_stream(STREAM_SQL, vec![], 4096).await.map_err(ReadError::into_error)?;
    let first = reader.next_batch().await.map_err(ReadError::into_error)?;
    assert!(!first.complete);
    let active = reader.observer.sample().await?.active;
    let sample = reader.observer.sample().await?;
    if let Some(phase) = &mut reader.phase {
        phase.observe(&sample);
    }
    tokio::time::sleep(Duration::from_millis(350)).await;
    let shutdown = reader.close().await;
    let quiet = reader.observer.is_quiet().await?;
    assert!(quiet);
    if shutdown == SourceShutdown::Unconfirmed {
        assert_no_followup(&mut reader).await;
    }
    Ok(json!({
        "case":"bounded-output-backpressure",
        "source_template":false,
        "native_active_after_first_batch":active,
        "first_rows":first.rows.len(),
        "shutdown":shutdown_state(shutdown),
        "independently_observed_quiet":quiet,
        "accounting":reader.report(),
    }))
}

async fn retired_refuses_rearm(
    pool: Arc<Pool>,
    port: u16,
    password: &str,
    database: &str,
) -> culprit_core::Result<Value> {
    let budget = CollectionBudget { max_queries: 1, ..Default::default() };
    let mut reader = crate::test_support::measured_read(pool, port, password, database, budget.clone()).await?;
    reader
        .query("SELECT /* culprit fixture exhausted budget */ 1 AS value", vec![], 1)
        .await
        .map_err(ReadError::into_error)?;
    assert!(reader.owner.measurement_window(budget).is_err(), "a retired query budget cannot be rearmed");
    assert_eq!(reader.queries(), 1);
    let shutdown = reader.close().await;
    Ok(json!({
        "case":"retired-budget-refuses-rearm",
        "source_template":false,
        "shutdown":shutdown_state(shutdown),
        "accounting":reader.report(),
    }))
}

async fn uncertain_refuses_rearm(
    pool: Arc<Pool>,
    port: u16,
    password: &str,
    database: &str,
) -> culprit_core::Result<Value> {
    let mut reader =
        crate::test_support::measured_read(pool.clone(), port, password, database, CollectionBudget::default()).await?;
    pool.mark_uncertain();
    assert!(
        reader.owner.measurement_window(CollectionBudget::default()).is_err(),
        "a prior uncertainty fence cannot be reset by instrumentation"
    );
    let shutdown = reader.close().await;
    assert_eq!(shutdown, SourceShutdown::Unconfirmed);
    assert_no_followup(&mut reader).await;
    Ok(json!({
        "case":"injected-prior-uncertainty-refuses-rearm",
        "source_template":false,
        "fault_injection":true,
        "shutdown":shutdown_state(shutdown),
        "accounting":reader.report(),
    }))
}

pub(crate) async fn assert_no_followup(reader: &mut MeasuredRead) {
    let queries = reader.queries();
    assert!(reader.owner.query("SELECT /* culprit fixture forbidden followup */ 1", vec![], 1).await.is_err());
    assert_eq!(reader.queries(), queries, "a retired or uncertain owner cannot admit follow-up native SQL");
}
