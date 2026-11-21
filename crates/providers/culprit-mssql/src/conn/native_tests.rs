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

#[tokio::test]
#[ignore = "requires explicit nonce-owned disposable loopback SQL source supervisor and native receipt directory; no live source"]
async fn owned_collection_acquisition_records_fresh_reuse_and_failed_login_without_borrowing_database_identity() {
    use crate::test_support::{OwnedSourceFixture, owned_case::finish_case};
    use culprit_collector::capture::CollectionCancellation;
    let cancellation = CollectionCancellation::default();
    let (expired, fixture) = finish_case(
        tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        &cancellation,
        OwnedSourceFixture::open("culprit-acquisition-observer-owned"),
    )
    .await;
    let fixture = fixture.expect("positive nonce-owned fixture admission");
    assert!(!expired);
    let (expired, result) = finish_case(
        tokio::time::Instant::now() + std::time::Duration::from_secs(20),
        &cancellation,
        acquisition_case(&fixture, &cancellation),
    )
    .await;
    result.expect("original observed native acquisitions and cleanup");
    assert!(!expired);
}

async fn acquisition_case(
    fixture: &crate::test_support::OwnedSourceFixture,
    cancellation: &culprit_collector::capture::CollectionCancellation,
) -> Result<()> {
    use culprit_core::telemetry::*;
    let budget = CollectionBudget {
        max_queries: 2,
        statement_timeout_ms: 1000,
        total_timeout_ms: 5000,
        ..CollectionBudget::default()
    };
    let mut originals = Vec::new();
    for _ in 0..2 {
        let read = acquisition_read(fixture.pool.clone(), budget.clone(), cancellation).await?;
        let rows = read.result.map_err(crate::read::ReadError::into_error)?;
        if read.shutdown != SourceShutdown::Quiescent
            || read.queries != 1
            || rows.len() != 1
            || crate::read::text(&rows[0], "original_database")? != fixture.database
        {
            return Err(crate::read::invalid_data());
        }
        originals.push(read.metrics);
    }
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    let mut config = crate::test_support::config(
        fixture.port,
        &fixture.database,
        "invalid-nonce-fixture-password",
        "culprit-acquisition-failed-owned",
    );
    config.authentication(tiberius::AuthMethod::sql_server(
        format!("missing_{}", fixture.database),
        "invalid-nonce-fixture-password",
    ));
    let failed = acquisition_read(Arc::new(Pool::new(config, None)), budget, cancellation).await?;
    let Err(error) = failed.result else { return Err(crate::read::invalid_data()) };
    if !matches!(error,crate::read::ReadError::Source(error) if error.code==ErrorCode::AuthFailed) {
        return Err(crate::read::invalid_data());
    }
    let fresh = originals[0].connection_work.as_ref().ok_or_else(crate::read::invalid_data)?;
    let reused = originals[1].connection_work.as_ref().ok_or_else(crate::read::invalid_data)?;
    let failure = failed.metrics.connection_work.as_ref().ok_or_else(crate::read::invalid_data)?;
    if !fresh.is_valid()
        || !reused.is_valid()
        || !failure.is_valid()
        || fresh.attempts[0].connection_use != ReadConnectionUse::FreshConnection
        || reused.attempts[0].connection_use != ReadConnectionUse::AuthenticatedReuse
        || !fresh.attempts[0].phases.iter().any(|phase| {
            phase.stage == ReadConnectionStage::HandshakeAndLogin && phase.outcome == ReadConnectionOutcome::Completed
        })
        || !reused.attempts[0]
            .phases
            .iter()
            .all(|phase| matches!(phase.stage, ReadConnectionStage::Acquire | ReadConnectionStage::FirstQuery))
        || failure.attempts[0].reader_generation.is_some()
        || !failure.attempts[0].phases.iter().any(|phase| {
            matches!(
                phase.outcome,
                ReadConnectionOutcome::Failed { code: ErrorCode::AuthFailed, native_code: Some(18456) }
            )
        })
        || failed.queries != 1
        || !failed.metrics.templates.is_empty()
        || failed.metrics.native_session_work.is_some()
        || failed.shutdown != SourceShutdown::Unconfirmed
    {
        return Err(crate::read::invalid_data());
    }
    let setup_batches = fresh.attempts[0]
        .phases
        .iter()
        .filter(|phase| {
            phase.stage == ReadConnectionStage::SessionSetup && phase.outcome == ReadConnectionOutcome::Completed
        })
        .count();
    let completed_queries = originals
        .iter()
        .filter_map(|metrics| metrics.connection_work.as_ref())
        .flat_map(|work| &work.attempts)
        .flat_map(|attempt| &attempt.phases)
        .filter(|phase| {
            phase.stage == ReadConnectionStage::FirstQuery && phase.outcome == ReadConnectionOutcome::Completed
        })
        .count();
    if setup_batches != 1 || completed_queries != 2 {
        return Err(crate::read::invalid_data());
    }
    crate::test_support::save_cost_receipt(
        &crate::test_support::receipt_directory(),
        "connection-acquisition-native",
        &serde_json::json!({"scope":"owned existing collection ReadSession and Pool hooks; no factory/cost qualification","fresh":fresh,"reused":reused,"failed":failure,"failed_owner_shutdown":"unconfirmed","reader_query_attempts":3,"successful_native_catalog_queries":completed_queries,"source_set_batches":setup_batches,"separately_paid_admission":"existing nonce fixture login/setup/guard","source_cost_qualified":false}),
    )?;
    Ok(())
}
struct AcquisitionRead {
    result: crate::read::ReadResult<Vec<crate::value::Rec>>,
    metrics: culprit_core::telemetry::ReadWorkMetrics,
    shutdown: culprit_core::telemetry::SourceShutdown,
    queries: u32,
}
async fn acquisition_read(
    pool: Arc<Pool>,
    budget: culprit_core::telemetry::CollectionBudget,
    cancellation: &culprit_collector::capture::CollectionCancellation,
) -> Result<AcquisitionRead> {
    use culprit_core::telemetry::CollectionStop;
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    let mut reader = crate::read::ReadSession::collection(pool, budget)?;
    let result=reader.query("SELECT /* culprit */ DB_NAME() AS original_database,CONVERT(nvarchar(128),SERVERPROPERTY('ProductVersion')) AS native_version",vec![],1).await;
    let shutdown = reader.close().await;
    Ok(AcquisitionRead {
        result,
        shutdown,
        queries: reader.queries(),
        metrics: reader.read_metrics().ok_or_else(crate::read::invalid_data)?,
    })
}
