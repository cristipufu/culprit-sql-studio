use super::*;
use crate::{cached_statements, relational_inputs::tests::binding_row, resources::test_support::FakeRead, value::Val};

fn original() -> (CachedStatementContext, i64) {
    let row = metadata::tests::row();
    (cached_statements::context(&row).unwrap(), crate::read::signed_integer(&row, "module_object_id").unwrap())
}
fn matched_observer(original: &CachedStatementContext, checks: usize) -> FakeRead {
    FakeRead::new(matched_replies(original, checks))
}
fn matched_replies(original: &CachedStatementContext, checks: usize) -> Vec<ReadResult<Vec<Rec>>> {
    (0..checks).flat_map(|_| [Ok(vec![metadata::tests::row()]), Ok(vec![binding_row(original)])]).collect()
}

#[tokio::test]
async fn original_payload_parameters_limits_and_source_counts_exclude_all_observer_work() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let rows = vec![Rec::of(&[("native", Val::Int(7))])];
    let mut source = FakeRead::new(vec![Ok(rows.clone())]);
    let mut observer = matched_observer(&original, 2);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    let sql = "SELECT /* culprit */ native_source";
    let result = trace.query(sql, vec![Parameter::Integer(7)], 2).await.unwrap();
    assert_eq!(crate::read::integer(&result[0], "native").unwrap(), 7);
    assert_eq!(trace.queries(), 1);
    assert_eq!(trace.observer.queries(), 4);
    assert_eq!(trace.source.calls[0].sql, sql);
    assert_eq!(trace.source.calls[0].limit, 2);
    assert!(matches!(trace.source.calls[0].parameters.as_slice(), [Parameter::Integer(7)]));
    assert_eq!(trace.observations[0].template_id, crate::statement_origin::template_id(sql));
    assert!(
        trace.observations[0].redacted_template.is_none(),
        "no invented source metric when the reader reports none"
    );
    assert_eq!(trace.observations[0].original_found_after, Some(true));
    trace.assign_phases(&[PhaseSpan { phase: MemoryPhase::SqlEnrichment, first_exclusive: 0, last_inclusive: 1 }]);
    assert!(matches!(trace.observations[0].phase, Some(MemoryPhase::SqlEnrichment)));
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn known_first_loss_stops_before_any_further_source_or_observer_dispatch() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Ok(vec![])]);
    let mut observer = FakeRead::new(vec![
        Ok(vec![metadata::tests::row()]),
        Ok(vec![binding_row(&original)]),
        Ok(vec![metadata::tests::missing_row()]),
    ]);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    assert!(trace.query("SELECT /* culprit */ original", vec![], 1).await.is_err());
    assert_eq!(trace.state, TraceState::OriginalNotFound);
    assert_eq!(trace.observations[0].source_ordinal, 1);
    assert!(trace.observations[0].source_query_completed);
    assert_eq!(trace.observations[0].original_found_after, Some(false));
    assert!(trace.query("SELECT /* culprit */ forbidden_followup", vec![], 1).await.is_err());
    assert_eq!((trace.source.calls.len(), trace.observer.calls.len()), (1, 3));
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn original_source_uncertainty_has_no_after_query_observer_or_later_dispatch() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))]);
    source.shutdown = SourceShutdown::Unconfirmed;
    let mut observer = matched_observer(&original, 1);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    assert!(matches!(
        trace.query("SELECT /* culprit */ original", vec![], 1).await,
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))
    ));
    assert_eq!(trace.state, TraceState::SourceFailed);
    assert!(trace.observations[0].original_found_after.is_none());
    assert!(trace.query("SELECT /* culprit */ forbidden_followup", vec![], 1).await.is_err());
    assert_eq!((trace.source.calls.len(), trace.observer.calls.len()), (1, 2));
    assert_eq!(trace.close().await, SourceShutdown::Unconfirmed);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn observer_permission_failure_never_becomes_known_absence_or_more_source_work() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Ok(vec![])]);
    let mut observer = FakeRead::new(vec![
        Ok(vec![metadata::tests::row()]),
        Ok(vec![binding_row(&original)]),
        Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "denied"))),
    ]);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    let error = trace.query("SELECT /* culprit */ original", vec![], 1).await.err().unwrap();
    assert!(matches!(error,ReadError::Source(error) if error.code==ErrorCode::PermissionDenied));
    assert_eq!(trace.state, TraceState::ObserverFailed);
    assert_eq!(trace.first_error.as_ref().unwrap().code, ErrorCode::PermissionDenied);
    assert!(trace.observations[0].original_found_after.is_none());
    assert!(trace.query("SELECT /* culprit */ forbidden_followup", vec![], 1).await.is_err());
    assert_eq!((trace.source.calls.len(), trace.observer.calls.len()), (1, 3));
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_cancelled_caller_starts_neither_baseline_nor_source_and_closes_once() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    cancellation.cancel();
    let mut source = FakeRead::new(vec![]);
    let mut observer = FakeRead::new(vec![]);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    assert!(trace.query("SELECT /* culprit */ forbidden", vec![], 1).await.is_err());
    assert_eq!(trace.state, TraceState::CallerCancelled);
    assert_eq!((trace.source.calls.len(), trace.observer.calls.len()), (0, 0));
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn cancellation_during_baseline_never_admits_the_first_original_source_dispatch() {
    struct CancellingObserver {
        reader: FakeRead,
        cancellation: CollectionCancellation,
    }
    #[async_trait::async_trait]
    impl NativeRead for CancellingObserver {
        async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
            let result = self.reader.query(sql, parameters, limit).await;
            if self.reader.queries() == 2 {
                self.cancellation.cancel();
            }
            result
        }
        fn queries(&self) -> u32 {
            self.reader.queries()
        }
        async fn close(&mut self) -> SourceShutdown {
            self.reader.close().await
        }
    }
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![]);
    let mut observer =
        CancellingObserver { reader: matched_observer(&original, 1), cancellation: cancellation.clone() };
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    assert!(trace.query("SELECT /* culprit */ forbidden", vec![], 1).await.is_err());
    assert!(trace.baseline_matched);
    assert_eq!(trace.state, TraceState::CallerCancelled);
    assert_eq!((trace.source.calls.len(), trace.observer.queries()), (0, 2));
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!((trace.source.closed, trace.observer.reader.closed), (1, 1));
}

#[tokio::test]
async fn a_source_panic_still_leaves_both_original_readers_available_for_settlement() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![]);
    let mut observer = matched_observer(&original, 1);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    let outcome = AssertUnwindSafe(trace.query("SELECT /* culprit */ panics", vec![], 1)).catch_unwind().await;
    assert!(outcome.is_err());
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn missing_baseline_is_a_failed_precondition_with_no_original_source_dispatch() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![]);
    let mut observer = FakeRead::new(vec![Ok(vec![metadata::tests::missing_row()])]);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    assert!(trace.query("SELECT /* culprit */ forbidden", vec![], 1).await.is_err());
    assert_eq!(trace.state, TraceState::BaselineFailed);
    assert!(!trace.baseline_matched);
    assert_eq!((trace.source.calls.len(), trace.observer.calls.len()), (0, 1));
    assert!(trace.observations.is_empty());
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn observer_shutdown_debt_is_not_repaired_by_a_quiescent_source() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Ok(vec![])]);
    let mut observer = matched_observer(&original, 3);
    observer.shutdown = SourceShutdown::Unconfirmed;
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    trace.query("SELECT /* culprit */ original", vec![], 1).await.unwrap();
    trace.observe_after_source_close().await.unwrap();
    assert_eq!(trace.post_close.as_ref().unwrap().original_found, Some(true));
    assert_eq!(trace.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(trace.close().await, SourceShutdown::Unconfirmed);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn post_close_check_retains_the_observer_and_never_reopens_the_original_source() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Ok(vec![])]);
    let mut observer = matched_observer(&original, 3);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    trace.query("SELECT /* culprit */ original", vec![], 1).await.unwrap();
    trace.observe_after_source_close().await.unwrap();
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 0));
    let after = trace.post_close.as_ref().unwrap();
    assert_eq!((after.source_queries, after.observer_queries_before, after.observer_queries_after), (1, 4, 6));
    assert_eq!(after.original_found, Some(true));
    assert_eq!(trace.state, TraceState::Observing);
    assert!(!trace.diagnostic_stop_injected);
    assert!(trace.observe_after_source_close().await.is_err());
    assert!(trace.query("SELECT /* culprit */ forbidden_reopen", vec![], 1).await.is_err());
    assert_eq!((trace.source.queries(), trace.observer.queries()), (1, 6));
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn post_close_known_loss_is_separate_from_an_injected_source_stop() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Ok(vec![])]);
    let mut replies = matched_replies(&original, 2);
    replies.push(Ok(vec![metadata::tests::missing_row()]));
    let mut observer = FakeRead::new(replies);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    trace.query("SELECT /* culprit */ original", vec![], 1).await.unwrap();
    trace.observe_after_source_close().await.unwrap();
    assert_eq!(trace.observations[0].original_found_after, Some(true));
    assert_eq!(trace.post_close.as_ref().unwrap().original_found, Some(false));
    assert_eq!(trace.state, TraceState::OriginalNotFound);
    assert!(!trace.diagnostic_stop_injected);
    assert!(trace.first_error.is_none());
    assert_eq!((trace.source.queries(), trace.observer.queries()), (1, 5));
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn unconfirmed_source_close_withholds_the_post_close_observer_dispatch() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Ok(vec![])]);
    source.shutdown = SourceShutdown::Unconfirmed;
    let mut observer = matched_observer(&original, 2);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    trace.query("SELECT /* culprit */ original", vec![], 1).await.unwrap();
    let error = trace.observe_after_source_close().await.unwrap_err();
    assert_eq!(error.detail, CollectionStop::CancellationUnconfirmed.error().detail);
    assert!(trace.post_close.is_none());
    assert_eq!((trace.source.queries(), trace.observer.queries()), (1, 4));
    assert_eq!(trace.close().await, SourceShutdown::Unconfirmed);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn post_close_observer_failure_preserves_the_first_error_and_unknown_presence() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Ok(vec![])]);
    let mut replies = matched_replies(&original, 2);
    replies.push(Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "denied"))));
    let mut observer = FakeRead::new(replies);
    observer.shutdown = SourceShutdown::Unconfirmed;
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    trace.query("SELECT /* culprit */ original", vec![], 1).await.unwrap();
    let error = trace.observe_after_source_close().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(trace.first_error.as_ref().unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(trace.state, TraceState::ObserverFailed);
    assert!(trace.post_close.as_ref().unwrap().original_found.is_none());
    assert!(!trace.diagnostic_stop_injected);
    assert_eq!(trace.close().await, SourceShutdown::Unconfirmed);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
    assert_eq!(trace.first_error.as_ref().unwrap().code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn post_close_observer_panic_still_settles_both_original_readers_once() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Ok(vec![])]);
    let mut observer = matched_observer(&original, 2);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    trace.query("SELECT /* culprit */ original", vec![], 1).await.unwrap();
    let outcome = AssertUnwindSafe(trace.observe_after_source_close()).catch_unwind().await;
    assert!(outcome.is_err());
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 0));
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}

#[tokio::test]
async fn cancellation_before_post_close_check_settles_source_without_admitting_observer_sql() {
    let (original, object) = original();
    let cancellation = CollectionCancellation::default();
    let mut source = FakeRead::new(vec![Ok(vec![])]);
    let mut observer = matched_observer(&original, 2);
    let mut trace = PhaseRead::new(&mut source, &mut observer, &original, object, &cancellation);
    trace.query("SELECT /* culprit */ original", vec![], 1).await.unwrap();
    cancellation.cancel();
    let error = trace.observe_after_source_close().await.unwrap_err();
    assert_eq!(error.detail, CollectionStop::Cancelled.error().detail);
    assert!(trace.post_close.as_ref().unwrap().original_found.is_none());
    assert_eq!((trace.source.queries(), trace.observer.queries()), (1, 4));
    assert_eq!(trace.close().await, SourceShutdown::Quiescent);
    assert_eq!((trace.source.closed, trace.observer.closed), (1, 1));
}
