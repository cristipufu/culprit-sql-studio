use super::*;
use crate::{read::session_work, resources::test_support::FakeRead, value::Val};
use std::sync::atomic::{AtomicUsize, Ordering};

struct OriginalRead {
    io: FakeRead,
    calls: Arc<AtomicUsize>,
    observations: usize,
}
#[async_trait::async_trait]
impl NativeRead for OriginalRead {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, row_limit: usize) -> ReadResult<Vec<Rec>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.io.query(sql, parameters, row_limit).await
    }
    fn queries(&self) -> u32 {
        self.io.queries()
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(17)
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        Some(ReadWorkMetrics { active_tasks: 0, untracked_tasks: 0, templates: vec![], native_session_work: None })
    }
    fn observe_session_work(&mut self, _: Observation) {
        self.observations += 1;
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

fn reader(replies: Vec<ReadResult<Vec<Rec>>>, shutdown: SourceShutdown) -> (GatedRead<OriginalRead>, GateControl) {
    let (pause, control) = dispatch_gate();
    let mut io = FakeRead::new(replies);
    io.shutdown = shutdown;
    let inner = OriginalRead { io, calls: Arc::new(AtomicUsize::new(0)), observations: 0 };
    (GatedRead { inner, pause: Some(pause), preparation: Preparation::Pending, original_requests: vec![] }, control)
}

fn preparation_point(reader: &mut GatedRead<OriginalRead>) {
    let authority =
        crate::local_source::test_azure_authority(&crate::activity::database_context::tests::row(5)).unwrap();
    session_work::observe(
        reader,
        &authority,
        &Rec::of(&[
            ("collector_session_id", Val::Int(71)),
            ("collector_login_without_offset", Val::Text("2020-01-01T05:30:00.003".into())),
            ("collector_cpu_ms", Val::Int(1)),
            ("collector_reads", Val::Int(2)),
            ("collector_logical_reads", Val::Int(3)),
            ("collector_dirty_marks", Val::Int(4)),
            ("collector_memory_pages", Val::Int(5)),
        ]),
    );
    assert!(matches!(reader.preparation, Preparation::Observed));
}

#[tokio::test]
async fn the_original_next_query_waits_for_release_and_preserves_read_ownership() {
    let (mut reader, control) = reader(vec![Ok(vec![]), Ok(vec![])], SourceShutdown::Unconfirmed);
    reader.observe_session_work(Observation::Started);
    reader.query("original authority", vec![], 2).await.unwrap();
    preparation_point(&mut reader);
    let calls = reader.inner.calls.clone();
    let metrics = reader.read_metrics();
    let (result, ()) =
        tokio::join!(reader.query("original task population", vec![Parameter::Integer(41)], 17), async {
            control.ready.await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "the task query must not precede gate release");
            control.released.send(true).unwrap();
        });
    result.unwrap();
    assert_eq!(reader.queries(), 2);
    assert_eq!(reader.connection_generation(), Some(17));
    assert_eq!(reader.read_metrics(), metrics);
    assert_eq!(reader.inner.observations, 2);
    let call = &reader.inner.io.calls[1];
    assert_eq!(call.sql, "original task population");
    assert_eq!(call.limit, 17);
    assert!(matches!(call.parameters.as_slice(), [Parameter::Integer(41)]));
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(reader.inner.io.closed, 1);
}

#[tokio::test]
async fn a_failed_gate_release_cancels_before_the_original_task_query_and_keeps_unconfirmed_close() {
    let (mut reader, control) = reader(vec![Ok(vec![])], SourceShutdown::Unconfirmed);
    reader.query("original authority", vec![], 2).await.unwrap();
    preparation_point(&mut reader);
    let cancellation = CollectionCancellation::default();
    let (query, release) = tokio::join!(
        reader.query("original task population", vec![], 17),
        release_when_ready(control, &cancellation, async { Err(invalid("scripted gate release failure")) })
    );
    assert!(matches!(query, Err(ReadError::Stopped(CollectionStop::Cancelled))));
    assert!(release.is_err());
    assert!(cancellation.is_cancelled());
    assert_eq!(reader.queries(), 1);
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
    assert_eq!(reader.inner.io.closed, 1);
}

#[tokio::test]
async fn a_missing_native_preparation_point_releases_the_gate_without_dispatching_a_task_query() {
    let (mut reader, control) = reader(vec![], SourceShutdown::Quiescent);
    reader.observe_session_work(Observation::Unavailable(NativeReadWorkIssue::MissingSession));
    assert!(reader.query("original task population", vec![], 17).await.is_err());
    let cancellation = CollectionCancellation::default();
    let releases = AtomicUsize::new(0);
    assert!(
        release_when_ready(control, &cancellation, async {
            releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    assert!(cancellation.is_cancelled());
    assert_eq!(reader.queries(), 0);
    assert_eq!(reader.inner.observations, 1);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn failed_preparation_still_releases_the_retained_gate_after_the_original_reader_closes() {
    let (mut reader, control) =
        reader(vec![Err(invalid("scripted native preparation failure").into())], SourceShutdown::Unconfirmed);
    assert!(reader.query("original authority", vec![], 2).await.is_err());
    assert_eq!(reader.close().await, SourceShutdown::Unconfirmed);
    let cancellation = CollectionCancellation::default();
    let releases = AtomicUsize::new(0);
    assert!(
        release_when_ready(control, &cancellation, async {
            releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    assert!(cancellation.is_cancelled());
    assert_eq!(reader.queries(), 1);
    assert_eq!(reader.inner.io.closed, 1);
}

#[tokio::test(start_paused = true)]
async fn a_dispatch_preparation_deadline_signals_cancellation_then_awaits_the_same_gate_release() {
    let (pause, control) = dispatch_gate();
    let cancellation = CollectionCancellation::default();
    let observed_cancellation = cancellation.clone();
    let (closing, close_started) = oneshot::channel();
    let (settle, settled) = oneshot::channel();
    let owner = tokio::spawn(async move {
        release_when_ready(control, &cancellation, async {
            closing.send(()).unwrap();
            settled.await.unwrap();
            Ok(())
        })
        .await
    });
    tokio::time::advance(DISPATCH_PREPARATION_TIMEOUT).await;
    close_started.await.unwrap();
    assert!(observed_cancellation.is_cancelled());
    assert!(!owner.is_finished(), "the deadline must not drop the retained gate cleanup owner");
    settle.send(()).unwrap();
    let error = owner.await.unwrap().unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    assert!(!pause.released.await.unwrap());
}

#[test]
fn native_population_receipts_distinguish_exact_matching_from_identity_filters_and_empty_populations() {
    use crate::activity::page_targets::{task_waits, tests::context};
    use crate::resources::test_support::replace;
    let task = task_waits::test_task_row("0000000000000101", 1);
    let facts = crate::activity::rows::record(&task).unwrap().unwrap().facts;
    let selector = ActivityRequestSelector { key: facts.key, statement_identity: facts.statement_identity.unwrap() };
    let changed_statement = replace(&task, &[("statement_start", Val::Int(2))]);
    let reused_request = replace(&task, &[("request_generation", Val::Text("2020-01-01T05:59:59".into()))]);
    let missing_statement = replace(&task, &[("sql_handle", Val::Null)]);
    let receipt = PopulationReceipt::from_rows(
        &[task, changed_statement, reused_request, missing_statement, context()],
        &[selector],
    );
    assert_eq!(receipt.returned_rows, 5);
    assert_eq!(receipt.context_only_rows, 1);
    assert_eq!(receipt.decoded_requests, 4);
    assert_eq!(receipt.native_task_rows, 4);
    assert_eq!(receipt.original_request_keys, 3);
    assert_eq!(receipt.matching_statement_tokens, 1);
    assert_eq!(receipt.different_statement_tokens, 1);
    assert_eq!(receipt.missing_statement_tokens, 1);
    assert_eq!(receipt.other_request_generations, 1);
    assert_eq!(receipt.decode_failures, 0);
    let empty = PopulationReceipt::from_rows(&[context()], &[]);
    assert_eq!(empty.context_only_rows, 1);
    assert_eq!(empty.decoded_requests, 0);
    assert_eq!(empty.native_task_rows, 0);
}
