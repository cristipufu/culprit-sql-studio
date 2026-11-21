//! Scripted protocol/ownership controls, not native cost qualification.
use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU32, Ordering},
};

pub(crate) struct ScriptedSession {
    io: FakeRead,
    queries: u32,
    generation: u64,
    metrics: crate::read_metrics::Metrics,
    work: session_work::SessionWork,
    admission: Arc<Admission>,
    pending: Option<(Option<String>, crate::read_metrics::Progress)>,
    batches: VecDeque<ReadBatch>,
    trace: Arc<Mutex<Vec<String>>>,
    closed: Arc<AtomicU32>,
    label: &'static str,
}
impl ScriptedSession {
    fn new(io: FakeRead, admission: Arc<Admission>, label: &'static str, trace: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            io,
            queries: 0,
            generation: 1,
            metrics: crate::read_metrics::Metrics::new(culprit_core::Engine::Mssql),
            work: session_work::SessionWork::default(),
            admission,
            pending: None,
            batches: VecDeque::new(),
            trace,
            closed: Arc::new(AtomicU32::new(0)),
            label,
        }
    }
    fn begin(&mut self, sql: &str) -> ReadResult<()> {
        self.admission.start().map_err(ReadError::Stopped)?;
        self.queries += 1;
        self.trace.lock().unwrap().push(format!("{}:{sql}", self.label));
        self.pending = Some((self.metrics.begin(sql), crate::read_metrics::Progress::default()));
        Ok(())
    }
    fn rows(&self, rows: &[Rec]) -> ReadResult<()> {
        for row in rows {
            let bytes = value_bytes(row);
            self.admission.charge(bytes).map_err(ReadError::Stopped)?;
            let (_, progress) = self.pending.as_ref().unwrap();
            progress.rows.fetch_add(1, Ordering::Relaxed);
            progress.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        }
        Ok(())
    }
    fn end(&mut self, failed: bool, unconfirmed: bool) {
        if let Some((id, progress)) = self.pending.take() {
            self.metrics.finish(
                id.as_deref(),
                Duration::ZERO,
                &progress,
                &ReadTaskOutcome { failed, unconfirmed, ..Default::default() },
            );
        }
    }
}
#[async_trait::async_trait]
impl NativeRead for ScriptedSession {
    async fn query(&mut self, sql: &str, parameters: Vec<Parameter>, limit: usize) -> ReadResult<Vec<Rec>> {
        self.begin(sql)?;
        let result = self.io.query(sql, parameters, limit).await;
        let result = result.and_then(|rows| {
            self.rows(&rows)?;
            Ok(rows)
        });
        self.end(result.is_err(), false);
        result
    }
    fn queries(&self) -> u32 {
        self.queries
    }
    fn connection_generation(&self) -> Option<u64> {
        Some(self.generation)
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        let mut metrics = self.metrics.snapshot()?;
        metrics.native_session_work = self.work.snapshot();
        Some(metrics)
    }
    fn observe_session_work(&mut self, observation: session_work::Observation) {
        self.work.observe(observation);
    }
    async fn close(&mut self) -> SourceShutdown {
        self.closed.fetch_add(1, Ordering::Relaxed);
        self.trace.lock().unwrap().push(format!("{}:close", self.label));
        self.end(true, self.io.shutdown == SourceShutdown::Unconfirmed);
        let shutdown = self.io.close().await;
        self.work.close(shutdown, self.generation);
        shutdown
    }
}
#[async_trait::async_trait]
impl NativeStreamRead for ScriptedSession {
    async fn begin_stream(&mut self, sql: &str, _: Vec<Parameter>, _: usize) -> ReadResult<()> {
        self.begin(sql)
    }
    async fn next_batch(&mut self) -> ReadResult<ReadBatch> {
        let batch = self.batches.pop_front().expect("one retained original native stream");
        self.rows(&batch.rows)?;
        if batch.complete {
            self.end(false, false);
        }
        Ok(batch)
    }
}
impl QualificationSession for ScriptedSession {
    fn accounting(&self) -> Option<ReadPhaseAccounting> {
        Some(ReadPhaseAccounting {
            queries: self.queries,
            metrics: Box::new(auxiliary_metrics(self.read_metrics()?)?),
            session_setup: Box::new(empty_metrics()),
        })
    }
    fn ready_for_transfer(&self) -> Result<()> {
        self.admission.remaining()?;
        if self.pending.is_some() || self.read_metrics().is_some_and(|metrics| matches!(metrics.native_session_work.as_deref(),Some(NativeSessionReadWork::MssqlSession(work)) if work.issue.is_some())) { Err(CollectionStop::CancellationUnconfirmed.error()) } else { Ok(()) }
    }
    fn into_source_phase(mut self) -> Self {
        self.trace.lock().unwrap().push(format!("{}:consumed-transfer", self.label));
        // Original query trace is retained. The caller sealed this preparation accounting before transfer.
        self.queries = 0;
        self.metrics = crate::read_metrics::Metrics::new(culprit_core::Engine::Mssql);
        self.work = session_work::SessionWork::default();
        self
    }
    fn session_work(&mut self) -> &mut session_work::SessionWork {
        &mut self.work
    }
}
const PREP: &str = "2020-01-01T08:00:59Z";
const BEFORE: &str = "2020-01-01T08:01:00Z";
const AFTER: &str = "2020-01-01T08:01:03Z";
fn login(id: i64) -> String {
    format!("2020-01-01T05:30:00.{:03}0000", id - 68)
}
fn authority(at: &str, id: i64, cpu: i64) -> Rec {
    let row = replace(&crate::activity::memory::tests::context(), &[("observed_at", Val::Text(at.into()))]);
    let mut fields = row.columns().map(|(key, value)| (key, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("collector_session_id", Val::Int(id)),
        ("collector_login_without_offset", Val::Text(login(id))),
        ("collector_cpu_ms", Val::Int(cpu)),
        ("collector_reads", Val::Int(9_007_199_254_740_993)),
        ("collector_logical_reads", Val::Int(200 + cpu)),
        ("collector_dirty_marks", Val::Int(3)),
        ("collector_memory_pages", Val::Int(32)),
    ]);
    Rec::of(&fields)
}
fn platform() -> Rec {
    replace(
        &crate::instance::test_support::metadata(),
        &[("edition", Val::Int(5)), ("machine_name", Val::Null), ("process_id", Val::Null)],
    )
}
fn sampled(at: &str, cpu: i64) -> Rec {
    Rec::of(&[
        ("observed_at", Val::Text(at.into())),
        ("cpu_ms", Val::Int(cpu)),
        ("reads", Val::Int(9_007_199_254_740_993)),
        ("logical_reads", Val::Int(200 + cpu)),
        ("dirty_marks", Val::Int(3)),
        ("memory_pages", Val::Int(32)),
        ("active", Val::Bool(false)),
        ("wait_type", Val::Null),
        ("blocking_session_id", Val::Null),
        ("granted_kb", Val::Null),
        ("requested_kb", Val::Null),
        ("used_kb", Val::Null),
    ])
}
pub(crate) fn scripted_reader(
    source_replies: Vec<ReadResult<Vec<Rec>>>,
    batches: Vec<ReadBatch>,
    budget: CollectionBudget,
) -> QualificationRead<ScriptedSession> {
    let admission = Arc::new(Admission::new(budget).unwrap());
    let trace = Arc::new(Mutex::new(vec![]));
    let mut replies = vec![
        Ok(vec![Rec::of(&[("session_id", Val::Int(71)), ("login_time", Val::Text(login(71)))])]),
        Ok(vec![platform()]),
        Ok(vec![authority(PREP, 71, 1)]),
    ];
    replies.extend(source_replies);
    let mut source = ScriptedSession::new(FakeRead::new(replies), admission.clone(), "source", trace.clone());
    source.batches = batches.into();
    let observer = ScriptedSession::new(
        FakeRead::new(vec![
            Ok(vec![platform()]),
            Ok(vec![authority(PREP, 72, 1)]),
            Ok(vec![sampled(BEFORE, 10)]),
            Ok(vec![platform()]),
            Ok(vec![authority(AFTER, 72, 2)]),
            Ok(vec![sampled(AFTER, 20)]),
        ]),
        admission.clone(),
        "observer",
        trace,
    );
    QualificationRead::with_sessions(source, observer, admission, "fixture.invalid".into(), 1433)
}
fn protocol_reader() -> QualificationRead<ScriptedSession> {
    scripted_reader(vec![Ok(vec![Rec::of(&[("result", Val::Int(42))])])], vec![], CollectionBudget::default())
}
fn body(metrics: &ReadWorkMetrics) -> &MssqlSessionReadWork {
    let NativeSessionReadWork::MssqlSession(work) = metrics.native_session_work.as_deref().unwrap();
    work
}

fn unconnected_qualification_session() -> ReadSession {
    let pool = Arc::new(Pool::new(tiberius::Config::new(), None));
    let admission = Arc::new(Admission::new(CollectionBudget::default()).unwrap());
    ReadSession::qualification(pool, admission)
}

#[test]
fn consumed_transfer_preserves_lifetime_connection_history_and_sealed_preparation_accounting() {
    let mut owner = unconnected_qualification_session();
    let pool = owner.pool.clone();
    let admission = owner.admission.as_ref().unwrap().clone();
    let deadline = owner.deadline;
    let budget = owner.budget.clone();
    let recorder = owner.connection_work.as_ref().unwrap().clone();
    let attempt = recorder.begin();
    let acquire = attempt.phase(ReadConnectionStage::Acquire, None);
    attempt.connection_use(ReadConnectionUse::FreshConnection);
    owner.connection_generation = 7;
    attempt.acquired(owner.connection_generation);
    owner.session_setup.as_ref().unwrap().begin("SET LOCK_TIMEOUT 2000 /* culprit */").unwrap().finish(true);
    acquire.finish(ReadConnectionOutcome::Completed);

    let preparation_sql = [
        "SELECT /* culprit */ @@SPID AS session_id",
        "SELECT /* culprit */ SERVERPROPERTY('EngineEdition') AS edition",
        "SELECT /* culprit */ SYSUTCDATETIME() AS observed_at",
    ];
    for (index, sql) in preparation_sql.iter().enumerate() {
        admission.start().unwrap();
        owner.queries += 1;
        let template = owner.metrics.begin(sql);
        let first_query =
            (index == 0).then(|| attempt.phase(ReadConnectionStage::FirstQuery, Some(digest(sql.as_bytes()))));
        owner.metrics.finish(
            template.as_deref(),
            Duration::ZERO,
            &ReadProgress::default(),
            &ReadTaskOutcome::default(),
        );
        if let Some(first_query) = first_query {
            first_query.finish(ReadConnectionOutcome::Completed);
        }
    }
    let preparation = owner.accounting().unwrap();
    assert_eq!(preparation.queries, 3);
    assert_eq!(preparation.metrics.templates.len(), 3);
    assert!(preparation.metrics.templates.iter().all(|template| template.completed_tasks == 1));
    assert_eq!(preparation.session_setup.templates.len(), 1);
    assert_eq!(admission.state.lock().unwrap().queries, 4);
    assert!(owner.first_query_attempt.is_none());
    assert!(owner.client.is_none(), "this component control creates no native session or readiness proof");

    let source = owner.into_source_phase();
    assert!(Arc::ptr_eq(&source.pool, &pool));
    assert!(Arc::ptr_eq(source.admission.as_ref().unwrap(), &admission));
    assert_eq!(source.deadline, deadline);
    assert_eq!(source.budget, budget);
    assert_eq!(source.connection_generation, 7);
    assert!(source.cancellation_confirmed);
    assert_eq!(source.queries(), 0);
    assert!(source.session_setup.is_none());
    assert!(source.first_query_attempt.is_none(), "the first preparation query remains the original FirstQuery");
    let source_metrics = source.read_metrics().unwrap();
    assert!(source_metrics.templates.is_empty());
    assert_eq!((source_metrics.active_tasks, source_metrics.untracked_tasks), (0, 0));
    assert!(source_metrics.native_session_work.is_none(), "transfer does not manufacture a native boundary");
    assert_eq!(source_metrics.connection_work, preparation.metrics.connection_work);
    let sealed = preparation.clone();

    // An external handle still observes the same lifetime recorder. Its uncertainty cannot be erased by transfer.
    drop(recorder.begin().phase(ReadConnectionStage::Acquire, None));
    let updated = source.read_metrics().unwrap();
    assert_eq!(updated.connection_work.as_ref().unwrap().attempted, 2);
    assert!(updated.has_unconfirmed_work());
    assert_eq!(preparation, sealed, "the sealed preparation snapshot does not alias the live recorder");
    assert_eq!(preparation.metrics.connection_work.as_ref().unwrap().attempted, 1);
    assert!(!preparation.metrics.has_unconfirmed_work());
    assert_eq!(admission.state.lock().unwrap().queries, 4, "the consumed transfer dispatches and charges nothing");
}

#[test]
fn consumed_transfer_preserves_the_pending_first_query_token_on_the_same_attempt() {
    let mut owner = unconnected_qualification_session();
    let recorder = owner.connection_work.as_ref().unwrap().clone();
    let attempt = recorder.begin();
    let acquire = attempt.phase(ReadConnectionStage::Acquire, None);
    attempt.connection_use(ReadConnectionUse::AuthenticatedReuse);
    owner.connection_generation = 3;
    attempt.acquired(owner.connection_generation);
    acquire.finish(ReadConnectionOutcome::Completed);
    owner.first_query_attempt = Some(attempt);
    let before = recorder.snapshot().unwrap();

    // This tests only consumed state transfer, without claiming this unconnected owner is ready for qualification.
    let mut source = owner.into_source_phase();
    let pending = source.first_query_attempt.take().expect("the held attempt survives its owner transfer");
    let template = digest(b"SELECT /* culprit */ pending_first_query");
    pending.phase(ReadConnectionStage::FirstQuery, Some(template.clone())).finish(ReadConnectionOutcome::Completed);
    assert!(source.first_query_attempt.is_none());
    let work = source.read_metrics().unwrap().connection_work.unwrap();
    assert!(work.is_valid());
    assert_eq!(work, Box::new(recorder.snapshot().unwrap()));
    assert_eq!(work.attempted, 1);
    assert_eq!(work.attempts[0].reader_generation.as_deref(), Some("3"));
    assert_eq!(work.attempts[0].phases.len(), 2);
    assert_eq!(work.attempts[0].phases[1].stage, ReadConnectionStage::FirstQuery);
    assert_eq!(work.attempts[0].phases[1].template_id.as_deref(), Some(template.as_str()));
    assert_eq!(before.attempts[0].phases.len(), 1);
    assert_eq!(source.queries(), 0, "observing a token does not fabricate a dispatched query");
}

#[tokio::test]
async fn before_first_dispatch_and_final_before_release_use_one_original_session_and_paid_phases() {
    let mut reader = protocol_reader();
    let trace = reader.observer.trace.clone();
    assert_eq!(reader.query("SELECT /* culprit */ 42 AS result", vec![], 1).await.unwrap().len(), 1);
    reader.finish_measurement().await.unwrap();
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let metrics = reader.read_metrics().unwrap();
    let work = body(&metrics);
    let interval = work.interval.as_ref().unwrap();
    assert_eq!(interval.initial.query_ordinal, 0);
    assert_eq!(interval.latest.as_ref().unwrap().query_ordinal, 1);
    assert_eq!(interval.session.id, "71");
    assert_eq!(interval.reader_generation, "1");
    assert_eq!(interval.initial.cpu_ms.as_deref(), Some("10"));
    assert_eq!(interval.latest.as_ref().unwrap().cpu_ms.as_deref(), Some("20"));
    let paid = work.qualification_work.as_ref().unwrap();
    assert!(paid.covers_source(&metrics, 1));
    assert_eq!(paid.preparation.queries, 3);
    assert_eq!(paid.observer.queries, 6);
    assert_eq!(reader.admission.state.lock().unwrap().queries, 10);
    assert_eq!(reader.owner.as_ref().unwrap().io.calls.len(), 4, "original preparation calls remain retained");
    let trace = trace.lock().unwrap();
    let sample = trace.iter().position(|sql| sql.starts_with("observer:SELECT") && sql.contains(" AS cpu_ms")).unwrap();
    let source = trace.iter().position(|sql| sql == "source:SELECT /* culprit */ 42 AS result").unwrap();
    assert!(sample < source);
    assert_eq!(trace[trace.len() - 2..], ["source:close", "observer:close"]);
    assert_eq!(reader.observer.closed.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn streaming_keeps_original_phase_open_until_terminal_batch_and_final_boundary() {
    let mut reader = scripted_reader(
        vec![],
        vec![
            ReadBatch { rows: vec![Rec::of(&[("value", Val::Int(1))])], complete: false },
            ReadBatch { rows: vec![], complete: true },
        ],
        CollectionBudget::default(),
    );
    reader.begin_stream("SELECT /* culprit */ value FROM native_catalog", vec![], 2).await.unwrap();
    assert!(!reader.next_batch().await.unwrap().complete);
    assert_eq!(reader.read_metrics().unwrap().active_tasks, 1);
    assert!(reader.next_batch().await.unwrap().complete);
    reader.finish_measurement().await.unwrap();
    reader.close().await;
    assert!(
        body(&reader.read_metrics().unwrap())
            .qualification_work
            .as_ref()
            .unwrap()
            .covers_source(&reader.read_metrics().unwrap(), 1)
    );
}
#[tokio::test]
async fn close_during_partial_stream_never_dispatches_an_observer_repair() {
    let mut reader =
        scripted_reader(vec![], vec![ReadBatch { rows: vec![], complete: false }], CollectionBudget::default());
    reader.begin_stream("SELECT /* culprit */ native_stream", vec![], 1).await.unwrap();
    let observed = reader.observer.queries();
    reader.close().await;
    assert_eq!(reader.observer.queries(), observed);
    let metrics = reader.read_metrics().unwrap();
    assert_eq!(body(&metrics).issue, Some(NativeReadWorkIssue::MissingFinalObservation));
    assert!(!body(&metrics).qualification_work.as_ref().unwrap().covers_source(&metrics, 1));
}
#[tokio::test]
async fn failed_source_and_failed_observer_forbid_final_queries_and_preserve_original_error() {
    for observer_failure in [false, true] {
        let mut reader = protocol_reader();
        if observer_failure {
            reader.observer.io = FakeRead::new(vec![Err(ReadError::Source(invalid_data()))]);
        } else {
            reader.owner.as_mut().unwrap().io = FakeRead::new(vec![
                Ok(vec![Rec::of(&[("session_id", Val::Int(71)), ("login_time", Val::Text(login(71)))])]),
                Ok(vec![platform()]),
                Ok(vec![authority(PREP, 71, 1)]),
                Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "private-source-error"))),
            ]);
        }
        let Err(error) = reader.query("SELECT /* culprit */ original_source", vec![], 1).await else {
            panic!("expected original failure")
        };
        if !observer_failure {
            assert_eq!(error.into_error().code, ErrorCode::PermissionDenied);
        }
        let calls = reader.observer.queries();
        assert!(reader.finish_measurement().await.is_err());
        reader.close().await;
        assert_eq!(reader.observer.queries(), calls);
        assert_eq!(reader.observer.closed.load(Ordering::Relaxed), 1);
    }
}
#[tokio::test]
async fn aggregate_budget_and_deadline_cannot_be_renewed_by_source_transition() {
    let mut reader = protocol_reader();
    reader.admission = Arc::new(Admission::new(CollectionBudget { max_queries: 4, ..Default::default() }).unwrap());
    reader.owner.as_mut().unwrap().admission = reader.admission.clone();
    reader.observer.admission = reader.admission.clone();
    assert!(reader.query("SELECT /* culprit */ original_source", vec![], 1).await.is_err());
    assert_eq!(reader.queries(), 0);
    assert_eq!(reader.admission.state.lock().unwrap().queries, 4);
    let observed = reader.observer.queries();
    reader.close().await;
    assert_eq!(reader.observer.queries(), observed);
    let mut reader = protocol_reader();
    reader.query("SELECT /* culprit */ 42 AS result", vec![], 1).await.unwrap();
    let deadline = reader.admission.deadline;
    reader.admission.state.lock().unwrap().stopped = Some(CollectionStop::TimedOut);
    assert!(reader.finish_measurement().await.is_err());
    assert_eq!(reader.admission.deadline, deadline);
    let observed = reader.observer.queries();
    reader.close().await;
    assert_eq!(reader.observer.queries(), observed);
}
#[tokio::test]
async fn interrupted_preparation_retains_its_accounting_and_closes_both_original_owners() {
    let mut reader = protocol_reader();
    reader.owner.as_mut().unwrap().io = FakeRead::new(vec![]).pause_on_query(1);
    assert!(
        tokio::time::timeout(Duration::from_millis(5), reader.query("SELECT /* culprit */ original_source", vec![], 1))
            .await
            .is_err()
    );
    let before = reader.observer.queries();
    assert!(reader.query("SELECT /* culprit */ replacement", vec![], 1).await.is_err());
    reader.close().await;
    assert_eq!(reader.queries(), 0);
    assert_eq!(reader.observer.queries(), before);
    assert_eq!(body(&reader.read_metrics().unwrap()).qualification_work.as_ref().unwrap().preparation.queries, 1);
    assert_eq!(reader.owner.as_ref().unwrap().closed.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn missing_repeated_or_interrupted_finish_cannot_be_repaired_by_cleanup() {
    let mut reader = protocol_reader();
    reader.query("SELECT /* culprit */ 42 AS result", vec![], 1).await.unwrap();
    reader.finish_measurement().await.unwrap();
    let observed = reader.observer.queries();
    assert!(reader.finish_measurement().await.is_err());
    reader.close().await;
    assert_eq!(reader.observer.queries(), observed);
    assert_eq!(body(&reader.read_metrics().unwrap()).issue, Some(NativeReadWorkIssue::InvalidObservation));
    let mut reader = protocol_reader();
    reader.query("SELECT /* culprit */ 42 AS result", vec![], 1).await.unwrap();
    reader.observer.io = FakeRead::new(vec![]).pause_on_query(1);
    assert!(tokio::time::timeout(Duration::from_millis(5), reader.finish_measurement()).await.is_err());
    let observed = reader.observer.queries();
    reader.close().await;
    assert_eq!(reader.observer.queries(), observed);
    assert_eq!(body(&reader.read_metrics().unwrap()).issue, Some(NativeReadWorkIssue::MissingFinalObservation));
}
#[test]
fn real_setup_guard_charges_same_parent_and_retains_unconfirmed_dropped_setup() {
    let admission = Arc::new(Admission::new(CollectionBudget { max_queries: 2, ..Default::default() }).unwrap());
    let source = SetupWork::new(admission.clone());
    let observer = SetupWork::new(admission.clone());
    source.begin("SET LOCK_TIMEOUT 2000").unwrap().finish(true);
    drop(observer.begin("SET LOCK_TIMEOUT 2000").unwrap());
    assert!(source.begin("SET LOCK_TIMEOUT 2000").is_err());
    assert_eq!(admission.state.lock().unwrap().queries, 2);
    assert_eq!(source.metrics.snapshot().unwrap().templates[0].completed_tasks, 1);
    assert_eq!(observer.metrics.snapshot().unwrap().templates[0].unconfirmed_cancellations, 1);
}
#[test]
fn shared_byte_limit_remains_sticky_for_all_owners() {
    let admission =
        Admission::new(CollectionBudget { page_bytes: 1024, total_bytes: 1024, ..Default::default() }).unwrap();
    admission.charge(700).unwrap();
    admission.charge(300).unwrap();
    assert_eq!(admission.charge(25), Err(CollectionStop::ByteLimit));
    assert_eq!(admission.start(), Err(CollectionStop::ByteLimit));
}

#[tokio::test]
async fn actual_azure_primary_and_native_namespace_remain_required_before_any_observer_work() {
    for (field, value) in
        [("updateability", Val::Text("READ_ONLY".into())), ("updateability", Val::Null), ("incarnation", Val::Null)]
    {
        let mut reader = protocol_reader();
        reader.owner.as_mut().unwrap().io = FakeRead::new(vec![
            Ok(vec![Rec::of(&[("session_id", Val::Int(71)), ("login_time", Val::Text(login(71)))])]),
            Ok(vec![platform()]),
            Ok(vec![replace(&authority(PREP, 71, 1), &[(field, value)])]),
        ]);
        assert!(reader.query("SELECT /* culprit */ source", vec![], 1).await.is_err(), "{field}");
        assert_eq!(reader.queries(), 0);
        assert_eq!(reader.observer.queries(), 0);
        reader.close().await;
        assert_eq!(reader.observer.queries(), 0);
    }
}
#[tokio::test]
async fn changed_observer_native_scope_or_transport_forbids_the_final_counter_sample() {
    for (field, value) in [
        ("updateability", Val::Text("READ_ONLY".into())),
        ("incarnation", Val::Text("foreign-logical-guid".into())),
        ("replica_id", Val::Text("41487244-730d-4b83-8e49-d482241c7288".into())),
        ("transport", Val::Null),
    ] {
        let mut reader = protocol_reader();
        reader.query("SELECT /* culprit */ 42 AS result", vec![], 1).await.unwrap();
        let after = if field == "transport" {
            reader.observer.generation = 2;
            authority(AFTER, 72, 2)
        } else {
            replace(&authority(AFTER, 72, 2), &[(field, value)])
        };
        reader.observer.io = FakeRead::new(vec![Ok(vec![platform()]), Ok(vec![after])]);
        assert!(reader.finish_measurement().await.is_err(), "{field}");
        assert_eq!(reader.observer.queries(), 5, "no final sample after contradictory native scope");
        reader.close().await;
        assert_eq!(reader.observer.queries(), 5);
        assert_ne!(body(&reader.read_metrics().unwrap()).issue, None);
    }
}
