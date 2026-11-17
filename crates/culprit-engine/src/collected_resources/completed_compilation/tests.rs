//! Controlled source facts pass through the real Outbox owner; they never qualify native cost or live timing.
use super::*;
use culprit_collector::{
    analytical::test_support::{
        compiler_admission_evidence, compiler_admission_investigation_fixture, query_compilation_evidence,
    },
    capture::{self, CollectionCancellation, SourceFactory},
    outbox::Outbox,
};
use culprit_core::{ErrorDetail, SourceState};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader, admission::SourceGate};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

#[derive(Clone)]
struct Frame {
    source: CollectionDescription,
    rows: Vec<TelemetryRecord>,
    source_calls: Arc<AtomicU32>,
}
#[async_trait::async_trait]
impl SourceFactory for Frame {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        Ok(Box::new(FrozenSource::new(self.clone(), request)?))
    }
}
#[async_trait::async_trait]
impl ObservationReader for Frame {
    async fn observe(&mut self, _: &CollectionRequest) -> Result<FrozenObservation> {
        self.source_calls.fetch_add(1, Ordering::SeqCst);
        Ok(FrozenObservation { description: self.source.clone(), records: self.rows.clone() })
    }
    async fn verify(&mut self, source: &CollectionDescription) -> Result<bool> {
        self.source_calls.fetch_add(1, Ordering::SeqCst);
        Ok(source == &self.source)
    }
    fn query_count(&self) -> u32 {
        0
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        None
    }
    async fn close(&mut self) -> SourceShutdown {
        SourceShutdown::Quiescent
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    outbox: Outbox,
    attempt: DiagnosticAttemptReceipt,
    source_calls: Arc<AtomicU32>,
}
impl Fixture {
    async fn source_read() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut outbox = Outbox::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
        outbox.recover().await.unwrap();
        let source_calls = Arc::new(AtomicU32::new(0));
        let fixture = compiler_admission_investigation_fixture();
        let mut source = fixture.baseline_source;
        let CollectionTarget::Database(database) = &mut source.target else { panic!() };
        database.incarnation = Some("controlled-source-read-query-store-database".into());
        let original = Frame { source, rows: vec![fixture.baseline_record.record], source_calls: source_calls.clone() };
        let (manifest, _) = capture_frame(&mut outbox, original, CollectionSelection::Activity).await;
        let input =
            outbox.diagnostic_input(&manifest.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
        let [original] = input.rows.as_slice() else { panic!() };
        let TelemetryRecord::Activity(actor) = &original.row.record else { panic!() };
        let read = NativeReadContext::from_activity(&input.source).unwrap();
        let execution = NativeSourceReadScope::from_context(&read).unwrap();
        let participant = ActivityRequestFocus {
            parent: original.reference.clone(),
            target: input.source.target.clone(),
            partition: input.source.partition.clone(),
            source: input.source.source.clone(),
            epoch: read.partition.native_epoch().unwrap().into(),
            database_writable: read.database_writable,
            original_observed_at: input.source.observation.activity().unwrap().at.clone(),
            original: actor.as_ref().clone(),
            statement_identity: actor.statement_identity.clone().unwrap(),
        };
        let (mut gateway, mut rows) = compiler_admission_evidence();
        gateway.target = input.source.target.clone();
        gateway.partition = input.source.partition.clone();
        let SourceObservation::FocusedCompilationAdmission { result, snapshot } = &mut gateway.observation else {
            panic!()
        };
        result.focus = participant.clone();
        for row in &mut rows {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            if let OperationalFact::CompilerWaiter(waiter) = &mut row.fact {
                waiter.request = actor.as_ref().clone();
                waiter.observed_at = snapshot.observed_from.clone();
                row.key = waiter.key().unwrap();
            }
        }
        let (manifest, _) = capture_frame(
            &mut outbox,
            Frame { source: gateway, rows, source_calls: source_calls.clone() },
            CollectionSelection::FocusedCompilationAdmission { focus: Box::new(participant) },
        )
        .await;
        let gateway =
            outbox.diagnostic_input(&manifest.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
        let waiter = gateway
            .rows
            .iter()
            .find(|row| {
                matches!(
                    row.row.record,
                    TelemetryRecord::Operational(OperationalObservation {
                        fact: OperationalFact::CompilerWaiter(_),
                        ..
                    })
                )
            })
            .unwrap();
        let prerequisite = CompilationAdmissionReport {
            observation: waiter.reference.clone(),
            gateways: gateway
                .rows
                .iter()
                .filter(|row| row.reference != waiter.reference)
                .map(|row| row.reference.clone())
                .collect(),
        };
        let focus = SourceReadFocus {
            basis: Box::new(original.reference.clone()),
            target: input.source.target.clone(),
            question: SourceReadQuestion::CompilationHistory,
            prerequisite: Box::new(prerequisite),
        };
        let (mut source, rows) = query_compilation_evidence();
        source.target = input.source.target.clone();
        let mut snapshot = source.observation.snapshot().unwrap().clone();
        snapshot.observed_from = "2020-01-01T12:00:02Z".into();
        snapshot.observed_until = "2020-01-01T12:00:03Z".into();
        source.observation = SourceObservation::SourceRead {
            focus_key: focus.key().unwrap(),
            execution: Box::new(NativeReadContext {
                target: source.target.clone(),
                partition: execution.partition.clone(),
                database_writable: Some(execution.database_writable),
                observed_from: snapshot.observed_from.clone(),
                observed_until: snapshot.observed_until.clone(),
            }),
            snapshot,
        };
        let (_, attempt) = capture_frame(
            &mut outbox,
            Frame { source, rows, source_calls: source_calls.clone() },
            CollectionSelection::SourceRead { focus: Box::new(focus), execution: Box::new(execution) },
        )
        .await;
        Self { _root: root, outbox, attempt, source_calls }
    }
    async fn ordinary(state: SourceState, dataset: SnapshotDataset) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut outbox = Outbox::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
        outbox.recover().await.unwrap();
        let (mut source, mut rows) = if dataset == SnapshotDataset::QueryCompilation {
            query_compilation_evidence()
        } else {
            culprit_collector::analytical::test_support::cached_runtime_evidence_cases().pop().unwrap()
        };
        let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
        if state == SourceState::Unavailable {
            rows.clear();
        }
        header.records = rows.len() as u32;
        let coverage = &mut header.sources[0];
        *coverage = match state {
            SourceState::Unavailable => StateSourceCoverage::unavailable(
                coverage.source.clone(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            ),
            _ => {
                StateSourceCoverage::observed(coverage.source.clone(), rows.len() as u32, state != SourceState::Partial)
            }
        };
        let source_calls = Arc::new(AtomicU32::new(0));
        let (_, attempt) = capture_frame(
            &mut outbox,
            Frame { source, rows, source_calls: source_calls.clone() },
            CollectionSelection::Snapshot { dataset },
        )
        .await;
        Self { _root: root, outbox, attempt, source_calls }
    }
    async fn read(&self, dataset: SnapshotDataset) -> CompletedSnapshot {
        self.outbox
            .delivery()
            .completed_snapshot(&self.attempt, dataset, Default::default(), &CollectionCancellation::default())
            .await
            .unwrap()
    }
}

async fn capture_frame(
    outbox: &mut Outbox,
    frame: Frame,
    selection: CollectionSelection,
) -> (CollectionManifest, DiagnosticAttemptReceipt) {
    let request =
        CollectionRequest { subject: CollectionSubject::Database, selection, budget: CollectionBudget::default() };
    let mut lease = outbox.reserve("controlled-originals", &request).await.unwrap();
    let capture = lease.identity().clone();
    let directory = lease.directory().to_owned();
    let manifest = capture::collect(
        &SourceGate::default(),
        &frame,
        request,
        &directory,
        capture.clone(),
        &CollectionCancellation::default(),
        Some(&mut lease),
    )
    .await
    .unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    lease.finish().await.unwrap();
    let attempt = outbox.attempt_receipt(&capture, None).await.unwrap();
    (manifest, attempt)
}

#[tokio::test]
async fn a_real_source_read_reuses_compilation_view_and_preserves_originals_without_execution_scope_transfer() {
    let fixture = Fixture::source_read().await;
    let read = fixture.read(SnapshotDataset::QueryCompilation).await;
    let source = read.source().clone();
    let rows = read.records().to_vec();
    let reference = read.reference(0).unwrap();
    let context = NativeReadContext::from_snapshot(&source).unwrap();
    assert_eq!(context.partition, SourcePartition::Target);
    assert_eq!(context.database_writable, None);
    assert!(source.observation.execution_context().unwrap().has_qualified_local_owner());
    let raw = rows
        .iter()
        .map(|row| match &row.record {
            TelemetryRecord::Operational(row) => row,
            _ => panic!(),
        })
        .collect::<Vec<_>>();
    let coverage = &source.observation.snapshot().unwrap().sources[0];
    let outcome = state::read_outcome(coverage, raw.len(), true).unwrap();
    let expected = culprit_checks::compilation::explain(CompilationEvidence {
        context: Some(&context),
        observations: &raw,
        source: Some(&outcome),
        definitions: None,
    })
    .unwrap();
    let calls = fixture.source_calls.load(Ordering::SeqCst);
    let analysis = completed_compilation(read).unwrap();
    assert_eq!(serde_json::to_value(analysis.report()).unwrap(), serde_json::to_value(expected).unwrap());
    assert_eq!(analysis.original().source(), &source);
    assert_eq!(analysis.original().records(), rows);
    assert_eq!(analysis.original().row_ordinals(), [0]);
    assert_eq!(analysis.original().reference(0).unwrap(), reference);
    assert_eq!(Some(analysis.original().capture().manifest_sha256.clone()), fixture.attempt.manifest_sha256);
    assert!(analysis.original().is_read_complete());
    assert!(analysis.report().definition_read.is_none());
    assert_eq!(fixture.source_calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn complete_local_body_keeps_native_partial_and_unavailable_outcomes_without_healthy_empty_reinterpretation() {
    for expected in [SourceState::Partial, SourceState::Unavailable] {
        let fixture = Fixture::ordinary(expected, SnapshotDataset::QueryCompilation).await;
        let read = fixture.read(SnapshotDataset::QueryCompilation).await;
        let calls = fixture.source_calls.load(Ordering::SeqCst);
        let analysis = completed_compilation(read).unwrap();
        assert_eq!(analysis.report().source_state, Some(expected));
        assert!(analysis.original().is_read_complete());
        assert_eq!(analysis.report().entries.is_empty(), expected == SourceState::Unavailable);
        assert_eq!(analysis.original().source().observation.snapshot().unwrap().sources[0].state, expected);
        assert_eq!(fixture.source_calls.load(Ordering::SeqCst), calls);
    }
}

#[tokio::test]
async fn another_valid_completed_dataset_cannot_be_reinterpreted_as_compiler_history() {
    let fixture = Fixture::ordinary(SourceState::Collected, SnapshotDataset::CachedRuntime).await;
    let read = fixture.read(SnapshotDataset::CachedRuntime).await;
    let calls = fixture.source_calls.load(Ordering::SeqCst);
    let error = completed_compilation(read).unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)));
    assert_eq!(fixture.source_calls.load(Ordering::SeqCst), calls);
}
