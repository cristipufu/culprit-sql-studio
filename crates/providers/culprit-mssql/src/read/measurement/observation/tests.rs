use super::*;
use std::collections::VecDeque;

struct Samples {
    remaining: VecDeque<Result<Sample>>,
    attempts: usize,
}
impl Samples {
    fn new(samples: impl IntoIterator<Item = Result<Sample>>) -> Self {
        Self { remaining: samples.into_iter().collect(), attempts: 0 }
    }
}
impl SampleSource for Samples {
    async fn sample(&mut self) -> Result<Sample> {
        self.attempts += 1;
        self.remaining.pop_front().expect("no observation is retried after its phase loses the observer")
    }
}

fn phase(sql: &str) -> Phase {
    Phase {
        sql: sql.into(),
        parameters: json!([]),
        row_limit: 64,
        before: Counters { cpu_ms: 1, logical_reads: 10, reads: 0, dirty_marks: 0 },
        started: Instant::now(),
        active_samples: 0,
        waits: Default::default(),
        blocking_samples: 0,
        maximum_sampled_granted_kb: None,
        maximum_sampled_requested_kb: None,
        maximum_sampled_used_kb: None,
        observer_error: None,
    }
}
fn sample(active: bool) -> Result<Sample> {
    Ok(Sample {
        observed_at: "2020-01-01T06:00:00Z".into(),
        counters: Counters { cpu_ms: 2, logical_reads: 12, reads: 0, dirty_marks: 0 },
        memory_pages: None,
        active,
        wait: None,
        blocker: None,
        granted_kb: None,
        requested_kb: None,
        used_kb: None,
    })
}
fn watch(sql: &str) -> TemplateObservation {
    TemplateObservation { id: digest(sql.as_bytes()), observed: Arc::new(AtomicBool::new(false)) }
}
async fn pending_native<T>(result: ReadResult<T>) -> ReadResult<T> {
    tokio::time::sleep(SAMPLE_INTERVAL * 3).await;
    result
}
fn native_error(result: ReadResult<()>) -> Error {
    result.expect_err("the native error or unavailable measurement must remain a failure").into_error()
}

#[tokio::test(start_paused = true)]
async fn one_statement_phase_and_exact_watch_span_stream_start_and_multiple_batches() {
    let sql = "SELECT /* culprit */ query_id FROM sys.query_store_query";
    let mut phase = phase(sql);
    let started = phase.started;
    let mut observer = Samples::new([sample(true), sample(true), sample(false), sample(true)]);
    let active = AtomicBool::new(false);
    let watch = watch(sql);
    await_observed(async { Ok(()) }, &mut observer, &mut phase, &active, Some(&watch)).await.unwrap();
    assert_eq!(observer.attempts, 0, "a fast stream start is not fabricated active evidence");
    for complete in [false, true] {
        let work = async {
            tokio::time::sleep(SAMPLE_INTERVAL * 2 + Duration::from_millis(1)).await;
            Ok(ReadBatch { rows: vec![], complete })
        };
        let batch = await_observed(work, &mut observer, &mut phase, &active, Some(&watch)).await.unwrap();
        assert_eq!(batch.complete, complete);
        assert_eq!(phase.started, started);
        assert_eq!(phase.sql, sql);
        assert_eq!(phase.row_limit, 64);
        assert_eq!(phase.before.logical_reads, 10);
    }
    assert_eq!(observer.attempts, 4);
    assert_eq!(phase.active_samples, 3);
    assert!(phase.observer_error.is_none());
    assert!(active.load(Ordering::Acquire));
    assert!(watch.was_active());
}

#[tokio::test(start_paused = true)]
async fn a_fast_completed_template_has_no_invented_active_or_grant_observation() {
    let sql = "SELECT /* culprit */ query_id FROM sys.query_store_query";
    let mut phase = phase(sql);
    let mut observer = Samples::new([]);
    let active = AtomicBool::new(false);
    let watch = watch(sql);
    await_observed(async { Ok(()) }, &mut observer, &mut phase, &active, Some(&watch)).await.unwrap();
    assert_eq!(observer.attempts, 0);
    assert!(!active.load(Ordering::Acquire));
    assert!(!watch.was_active());
    assert_eq!(phase.maximum_sampled_granted_kb, None);
}

#[tokio::test(start_paused = true)]
async fn streaming_activity_for_another_template_cannot_mark_the_selected_watch() {
    let mut phase = phase("SELECT /* culprit */ query_id FROM sys.query_store_query");
    let watch = watch("SELECT /* culprit */ plan_id FROM sys.query_store_plan");
    let mut observer = Samples::new([sample(true), sample(true), sample(true)]);
    let active = AtomicBool::new(false);
    await_observed(pending_native(Ok(())), &mut observer, &mut phase, &active, Some(&watch)).await.unwrap();
    assert!(active.load(Ordering::Acquire));
    assert!(phase.active_samples > 0);
    assert!(!watch.was_active());
}

#[tokio::test(start_paused = true)]
async fn observer_loss_awaits_the_same_native_result_without_cancelling_it() {
    let mut phase = phase("SELECT /* culprit */ query_id FROM sys.query_store_query");
    let mut observer = Samples::new([Err(CollectionStop::SourceChanged.error())]);
    let active = AtomicBool::new(false);
    let finished = AtomicBool::new(false);
    let work = async {
        tokio::time::sleep(SAMPLE_INTERVAL * 4).await;
        finished.store(true, Ordering::Release);
        Err::<(), _>(ReadError::Stopped(CollectionStop::ByteLimit))
    };
    let result = await_observed(work, &mut observer, &mut phase, &active, None).await;
    assert!(finished.load(Ordering::Acquire), "instrumentation cannot drop the original pending body");
    assert_eq!(CollectionStop::from_error(&native_error(result)), CollectionStop::ByteLimit);
    assert_eq!(observer.attempts, 1);
    assert_eq!(CollectionStop::from_error(phase.observer_error.as_ref().unwrap()), CollectionStop::SourceChanged);
}

#[tokio::test(start_paused = true)]
async fn a_partial_stream_batch_keeps_observer_loss_until_the_original_terminal_failure() {
    let mut phase = phase("SELECT /* culprit */ query_id FROM sys.query_store_query");
    let mut observer = Samples::new([Err(CollectionStop::QueryLimit.error())]);
    let active = AtomicBool::new(false);
    let first = pending_native(Ok(ReadBatch { rows: vec![], complete: false }));
    let first = await_observed(first, &mut observer, &mut phase, &active, None).await.unwrap();
    assert!(!first.complete);
    let first_error = phase.observer_error.clone().unwrap();
    let final_result = pending_native(Err::<(), _>(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)));
    let native = await_observed(final_result, &mut observer, &mut phase, &active, None).await;
    assert_eq!(observer.attempts, 1, "neither a later batch nor a new polling call restarts sampling");
    let result = measured_result(native, Err(first_error), Some(SourceShutdown::Unconfirmed));
    assert_eq!(CollectionStop::from_error(&native_error(result)), CollectionStop::CancellationUnconfirmed);
}

#[tokio::test(start_paused = true)]
async fn observer_loss_during_stream_start_remains_sticky_for_the_following_batch() {
    let mut phase = phase("SELECT /* culprit */ query_id FROM sys.query_store_query");
    let mut observer = Samples::new([Err(CollectionStop::SourceChanged.error())]);
    let active = AtomicBool::new(false);
    await_observed(pending_native(Ok(())), &mut observer, &mut phase, &active, None).await.unwrap();
    let native = await_observed(pending_native(Ok(())), &mut observer, &mut phase, &active, None).await;
    assert_eq!(observer.attempts, 1);
    let result = measured_result(native, Err(phase.observer_error.clone().unwrap()), Some(SourceShutdown::Quiescent));
    assert_eq!(CollectionStop::from_error(&native_error(result)), CollectionStop::SourceChanged);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_pending_borrow_retains_its_statement_phase_for_owner_close() {
    let sql = "SELECT /* culprit */ query_id FROM sys.query_store_query";
    let mut phase = phase(sql);
    let mut observer = Samples::new([Err(CollectionStop::SourceChanged.error())]);
    let active = AtomicBool::new(false);
    {
        let work = await_observed(std::future::pending::<ReadResult<()>>(), &mut observer, &mut phase, &active, None);
        tokio::pin!(work);
        tokio::select! {
            result = &mut work => panic!("the native body is still pending: {result:?}"),
            _ = tokio::time::sleep(SAMPLE_INTERVAL * 4) => {},
        }
    }
    assert_eq!(observer.attempts, 1);
    assert_eq!(phase.sql, sql);
    assert_eq!(CollectionStop::from_error(phase.observer_error.as_ref().unwrap()), CollectionStop::SourceChanged);
    assert_eq!(phase.maximum_sampled_granted_kb, None);
}

#[test]
fn original_native_stops_and_permission_failure_outrank_lost_observation() {
    for stop in [
        CollectionStop::ByteLimit,
        CollectionStop::RowLimit,
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let native = Err::<(), _>(ReadError::Stopped(stop));
        let result =
            measured_result(native, Err(CollectionStop::SourceChanged.error()), Some(SourceShutdown::Quiescent));
        assert_eq!(CollectionStop::from_error(&native_error(result)), stop);
    }
    let denied = Error::new(ErrorCode::PermissionDenied, "native permission failure");
    let result = measured_result(
        Err::<(), _>(ReadError::Source(denied)),
        Err(CollectionStop::SourceChanged.error()),
        Some(SourceShutdown::Quiescent),
    );
    assert_eq!(native_error(result).code, ErrorCode::PermissionDenied);
}

#[test]
fn unconfirmed_settlement_outranks_native_stops_and_source_failures() {
    for stop in [CollectionStop::TimedOut, CollectionStop::ByteLimit] {
        for lost_observer in [false, true] {
            let accounting = if lost_observer { Err(CollectionStop::SourceChanged.error()) } else { Ok(()) };
            let result =
                measured_result(Err::<(), _>(ReadError::Stopped(stop)), accounting, Some(SourceShutdown::Unconfirmed));
            assert_eq!(CollectionStop::from_error(&native_error(result)), CollectionStop::CancellationUnconfirmed);
        }
    }
    for code in [ErrorCode::AuthFailed, ErrorCode::PermissionDenied] {
        for lost_observer in [false, true] {
            let error = Error::new(code, "native source failure");
            let accounting = if lost_observer { Err(CollectionStop::SourceChanged.error()) } else { Ok(()) };
            let result = measured_result(
                Err::<(), _>(ReadError::Source(error.clone())),
                accounting.clone(),
                Some(SourceShutdown::Unconfirmed),
            );
            assert_eq!(CollectionStop::from_error(&native_error(result)), CollectionStop::CancellationUnconfirmed);
            let confirmed =
                measured_result(Err::<(), _>(ReadError::Source(error)), accounting, Some(SourceShutdown::Quiescent));
            assert_eq!(native_error(confirmed).code, code);
        }
    }
}

#[test]
fn unconfirmed_settlement_outranks_instrumentation_after_native_success() {
    let result = measured_result(Ok(()), Err(CollectionStop::SourceChanged.error()), Some(SourceShutdown::Unconfirmed));
    assert_eq!(CollectionStop::from_error(&native_error(result)), CollectionStop::CancellationUnconfirmed);
}

#[test]
fn a_successful_unmeasured_native_body_is_not_a_successful_measurement() {
    for shutdown in [None, Some(SourceShutdown::Quiescent)] {
        let result = measured_result(Ok(()), Err(CollectionStop::SourceChanged.error()), shutdown);
        assert_eq!(CollectionStop::from_error(&native_error(result)), CollectionStop::SourceChanged);
    }
    assert!(measured_result(Ok(()), Ok(()), None).is_ok());
}

#[tokio::test]
async fn final_sampling_waits_for_the_same_original_task_and_never_turns_quiet_into_acknowledgement() {
    for (stop, shutdown) in [
        (CollectionStop::Cancelled, SourceShutdown::Quiescent),
        (CollectionStop::CancellationUnconfirmed, SourceShutdown::Unconfirmed),
    ] {
        let pool = Arc::new(Pool::new(tiberius::Config::new(), None));
        let mut owner = ReadSession::new(pool, CollectionBudget::default());
        let (release, waiting) = tokio::sync::oneshot::channel::<()>();
        owner.active = Some(tokio::spawn(async move {
            waiting.await.unwrap();
            Err(ReadError::Stopped(stop))
        }));
        let mut observer = Samples::new([sample(false)]);
        let mut final_point = Box::pin(settled_sample(&mut owner, &mut observer));
        assert!(matches!(futures_util::poll!(final_point.as_mut()), std::task::Poll::Pending));
        drop(final_point);
        assert!(owner.active.is_some(), "cancelled finalization retains the original task for its owner");
        assert_eq!(observer.attempts, 0, "no native counter is observed ahead of original task settlement");
        release.send(()).unwrap();
        assert!(!settled_sample(&mut owner, &mut observer).await.unwrap().active);
        assert_eq!(observer.attempts, 1);
        assert!(owner.active.is_none());
        assert_eq!(owner.close().await, shutdown);
        assert_eq!(owner.queries(), 0, "neither settlement nor its observer dispatches replacement source SQL");
    }
}
