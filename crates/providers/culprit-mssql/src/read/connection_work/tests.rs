use super::*;
use culprit_core::{ErrorCode, telemetry::*};
#[tokio::test]
async fn original_login_failure_keeps_sanitized_code_and_no_successful_generation_or_error_text() {
    let recorder = ReadConnectionRecorder::default();
    let attempt = recorder.begin();
    let acquire = attempt.phase(ReadConnectionStage::Acquire, None);
    attempt.connection_use(ReadConnectionUse::FreshConnection);
    let result: Result<()> = core_phase(Some(&attempt), ReadConnectionStage::RouteSelection, async {
        Err(culprit_core::Error::new(ErrorCode::ConnectionFailed, "private password=secret route.invalid"))
    })
    .await;
    acquire.finish(ReadConnectionOutcome::Failed { code: result.unwrap_err().code, native_code: None });
    let work = recorder.snapshot().unwrap();
    assert!(work.is_valid());
    assert!(work.attempts[0].reader_generation.is_none());
    let text = serde_json::to_string(&work).unwrap();
    assert!(!text.contains("secret") && !text.contains("route.invalid"));
}
#[tokio::test]
async fn native_typed_redirect_is_observed_without_persisting_the_routed_host() {
    let recorder = ReadConnectionRecorder::default();
    let attempt = recorder.begin();
    let acquire = attempt.phase(ReadConnectionStage::Acquire, None);
    attempt.connection_use(ReadConnectionUse::FreshConnection);
    let result: tiberius::Result<()> = native_phase(Some(&attempt), ReadConnectionStage::HandshakeAndLogin, async {
        Err(tiberius::error::Error::Routing { host: "private-worker.invalid".into(), port: 11001 })
    })
    .await;
    assert!(result.is_err());
    acquire.finish(ReadConnectionOutcome::Failed { code: ErrorCode::ConnectionFailed, native_code: None });
    let work = recorder.snapshot().unwrap();
    assert!(work.is_valid());
    assert_eq!(work.attempts[0].phases[1].outcome, ReadConnectionOutcome::Redirected);
    assert!(!serde_json::to_string(&work).unwrap().contains("private-worker"));
}
#[tokio::test]
async fn dropping_an_inflight_original_future_records_unconfirmed_instead_of_a_completed_login() {
    use futures_util::FutureExt;
    let recorder = ReadConnectionRecorder::default();
    let attempt = recorder.begin();
    let acquire = attempt.phase(ReadConnectionStage::Acquire, None);
    let mut future =
        Box::pin(native_phase::<()>(Some(&attempt), ReadConnectionStage::HandshakeAndLogin, std::future::pending()));
    assert!(future.as_mut().now_or_never().is_none());
    drop(future);
    drop(acquire);
    let work = recorder.snapshot().unwrap();
    assert!(work.is_valid());
    assert_eq!(work.unconfirmed_phases, 2);
    assert!(work.has_unconfirmed_work());
}
#[test]
fn settled_query_stop_preserves_owner_reason_while_missing_acknowledgement_stays_unconfirmed() {
    let result: ReadResult<()> = Err(ReadError::Stopped(CollectionStop::Cancelled));
    assert_eq!(outcome(&result), ReadConnectionOutcome::Stopped { reason: CollectionStop::Cancelled });
    let result: ReadResult<()> = Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed));
    assert_eq!(outcome(&result), ReadConnectionOutcome::Unconfirmed);
    let result: ReadResult<()> = Err(ReadError::Source(crate::read::invalid_data()));
    assert_eq!(outcome(&result), ReadConnectionOutcome::Failed { code: ErrorCode::QueryFailed, native_code: None });
}

#[test]
fn observing_collections_never_changes_the_caller_setup_reservation_or_interactive_reader() {
    let pool = std::sync::Arc::new(crate::conn::Pool::new(tiberius::Config::new(), None));
    let tight = CollectionBudget { max_queries: 1, ..CollectionBudget::default() };
    assert!(crate::read::ReadSession::collection(pool.clone(), tight).is_err());
    let source = crate::read::ReadSession::collection(
        pool.clone(),
        CollectionBudget { max_queries: 2, ..CollectionBudget::default() },
    )
    .unwrap();
    assert_eq!(source.budget.max_queries, 1);
    assert!(source.connection_work.is_some());
    assert!(source.read_metrics().unwrap().connection_work.is_none());
    let application = crate::read::ReadSession::application(pool);
    assert!(application.connection_work.is_none());
    assert_eq!(source.queries(), 0);
}
