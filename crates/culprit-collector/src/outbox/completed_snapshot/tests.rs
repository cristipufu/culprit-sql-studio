use super::*;
use crate::{
    analytical::test_support::cached_runtime_evidence_cases,
    capture::{self, SourceFactory},
};
use culprit_read::admission::SourceGate;
use std::path::PathBuf;

#[derive(Clone)]
struct FixtureSource {
    source: CollectionDescription,
    records: Vec<TelemetryRecord>,
    shutdown: SourceShutdown,
}
#[async_trait::async_trait]
impl SourceFactory for FixtureSource {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        assert!(self.source.matches_request(request));
        Ok(Box::new(self.clone()))
    }
}
#[async_trait::async_trait]
impl TelemetrySource for FixtureSource {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        Ok(self.source.clone())
    }
    fn query_count(&self) -> u32 {
        0
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        assert_eq!(stream, TelemetryStream::OperationalSamples);
        assert!(cursor.is_none());
        Ok(CollectionRead::Page { records: std::mem::take(&mut self.records), next_cursor: None })
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        assert!(self.records.is_empty());
        Ok(SourceCompletion::Verified)
    }
    async fn close(&mut self) -> SourceShutdown {
        self.shutdown
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    outbox: super::super::Outbox,
    directory: PathBuf,
    manifest: CollectionManifest,
    attempt: DiagnosticAttemptReceipt,
}
impl Fixture {
    async fn new(rows: u32, partial: bool) -> Self {
        Self::with_shutdown(rows, partial, SourceShutdown::Quiescent).await
    }
    async fn with_shutdown(rows: u32, partial: bool, shutdown: SourceShutdown) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut outbox = super::super::Outbox::create(&temp.path().join("outbox"), OutboxLimits::default()).unwrap();
        let (mut source, first) = cached_runtime_evidence_cases().pop().unwrap();
        let records = (0..rows)
            .map(|index| {
                let mut record = first[0].clone();
                let TelemetryRecord::Operational(row) = &mut record else { panic!() };
                let OperationalFact::MssqlCachedRuntime(sample) = &mut row.fact else { panic!() };
                sample.context.identity.plan_generation = (index + 1).to_string();
                row.key = sample.context.identity.key().unwrap();
                record
            })
            .collect::<Vec<_>>();
        let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
        header.records = rows;
        header.sources[0] = StateSourceCoverage::observed(header.sources[0].source.clone(), rows, !partial);
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::CachedRuntime },
            budget: CollectionBudget::default(),
        };
        assert!(source.matches_request(&request));
        let mut lease = outbox.reserve("owned-source", &request).await.unwrap();
        let directory = lease.directory().to_owned();
        let capture = lease.identity().clone();
        let factory = FixtureSource { source, records, shutdown };
        let manifest = capture::collect(
            &SourceGate::default(),
            &factory,
            request,
            &directory,
            capture,
            &CollectionCancellation::default(),
            Some(&mut lease),
        )
        .await
        .unwrap();
        assert_eq!(
            manifest.status,
            match shutdown {
                SourceShutdown::Quiescent => CollectionStatus::Complete,
                SourceShutdown::Unconfirmed => CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed),
            }
        );
        lease.finish().await.unwrap();
        let attempt = outbox.attempt_receipt(&manifest.capture, None).await.unwrap();
        assert!(attempt.manifest_sha256.is_some());
        Self { _temp: temp, outbox, directory, manifest, attempt }
    }
    async fn read(&self, budget: CompletedSnapshotBudget) -> Result<CompletedSnapshot> {
        self.outbox
            .delivery()
            .completed_snapshot(
                &self.attempt,
                SnapshotDataset::CachedRuntime,
                budget,
                &CollectionCancellation::default(),
            )
            .await
    }
    fn bytes(&self) -> u64 {
        std::fs::metadata(self.directory.join("manifest.json")).unwrap().len()
            + self.manifest.files.iter().map(|file| u64::from(file.bytes)).sum::<u64>()
    }
    fn body(&self) -> PathBuf {
        self.directory.join(&self.manifest.files[0].file)
    }
    async fn state(&self) -> OutboxCaptureState {
        self.outbox.status(1, None).await.unwrap().entries[0].state
    }
}

#[tokio::test]
async fn a_real_ready_capture_returns_original_canonical_rows_positions_and_exact_single_pass_bytes() {
    let fixture = Fixture::new(2, false).await;
    let read = fixture.read(CompletedSnapshotBudget::default()).await.unwrap();
    assert_eq!(read.bytes_read(), fixture.bytes());
    assert_eq!(read.capture().capture, fixture.attempt.capture);
    assert_eq!(Some(read.capture().manifest_sha256.clone()), fixture.attempt.manifest_sha256);
    assert_eq!(read.records().len(), 2);
    assert_eq!(read.row_ordinals(), [0, 1]);
    assert!(read.is_read_complete());
    for (index, row) in read.records().iter().enumerate() {
        read.reference(index).unwrap().validate_canonical_record(read.source(), &row.record).unwrap();
    }
    let references = (0..2).map(|index| read.reference(index).unwrap()).collect::<Vec<_>>();
    let carrier = read.into_cached_runtime_read().unwrap();
    assert!(carrier.is_valid());
    assert_eq!(carrier.captured_references().unwrap(), Some(references));
    let encoded: CachedRuntimeRead = serde_json::from_slice(&serde_json::to_vec(&carrier).unwrap()).unwrap();
    assert_eq!(encoded, carrier);
    assert!(matches!(encoded.origin, CachedRuntimeReadOrigin::Captured { .. }));
}

#[tokio::test]
async fn a_fully_read_partial_native_population_is_not_a_missing_local_body_or_a_healthy_empty_read() {
    let fixture = Fixture::new(1, true).await;
    let read = fixture.read(CompletedSnapshotBudget::default()).await.unwrap();
    assert!(read.is_read_complete());
    let header = read.source().observation.snapshot().unwrap();
    assert_eq!(header.records, 1);
    assert_eq!(header.sources[0].state, culprit_core::SourceState::Partial);
    assert!(!header.sources[0].complete_scope);
    let carrier = read.into_cached_runtime_read().unwrap();
    assert!(carrier.read_complete && carrier.is_valid());
    assert_eq!(carrier.description.observation.snapshot().unwrap().records, 1);
}

#[tokio::test]
async fn byte_and_row_refusals_return_no_truncated_success_and_leave_the_valid_capture_ready() {
    let fixture = Fixture::new(2, false).await;
    let exact = CompletedSnapshotBudget { max_bytes: fixture.bytes(), ..Default::default() };
    assert_eq!(fixture.read(exact).await.unwrap().bytes_read(), fixture.bytes());
    for budget in [
        CompletedSnapshotBudget { max_bytes: fixture.bytes() - 1, ..exact },
        CompletedSnapshotBudget { max_rows: 1, ..exact },
    ] {
        let error = fixture.read(budget).await.unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Capacity)));
        assert_eq!(fixture.state().await, OutboxCaptureState::Ready);
    }
    assert_eq!(fixture.read(exact).await.unwrap().records().len(), 2);
}

#[tokio::test]
async fn source_selection_epoch_and_manifest_substitution_cannot_rebind_an_exact_attempt() {
    let fixture = Fixture::new(1, false).await;
    let mut changed_epoch = fixture.attempt.clone();
    changed_epoch.capture.revision.as_mut().unwrap().owner_epoch = "999".into();
    let mut changed_manifest = fixture.attempt.clone();
    changed_manifest.manifest_sha256 = Some(digest(b"foreign-manifest"));
    for attempt in [changed_epoch, changed_manifest] {
        let error = fixture
            .outbox
            .delivery()
            .completed_snapshot(
                &attempt,
                SnapshotDataset::CachedRuntime,
                CompletedSnapshotBudget::default(),
                &CollectionCancellation::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Conflict)));
        assert_eq!(fixture.state().await, OutboxCaptureState::Ready);
    }
    let error = fixture
        .outbox
        .delivery()
        .completed_snapshot(
            &fixture.attempt,
            SnapshotDataset::QueryCompilation,
            CompletedSnapshotBudget::default(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)));
    assert_eq!(fixture.state().await, OutboxCaptureState::Ready);

    let mut changed = fixture.manifest.clone();
    changed.directory_synced = !changed.directory_synced;
    std::fs::write(fixture.directory.join("manifest.json"), serde_json::to_vec(&changed).unwrap()).unwrap();
    let error = fixture.read(CompletedSnapshotBudget::default()).await.unwrap_err();
    assert_eq!(
        error.detail,
        Some(ErrorDetail::Outbox(OutboxProblem::Integrity)),
        "changed committed bytes are corruption, distinct from a caller supplying another manifest digest"
    );
    assert_eq!(fixture.state().await, OutboxCaptureState::Quarantined);
}

#[tokio::test]
async fn later_local_body_changes_cannot_rewrite_a_previously_authenticated_loaded_value() {
    let fixture = Fixture::new(1, false).await;
    let read = fixture.read(CompletedSnapshotBudget::default()).await.unwrap();
    let original = serde_json::to_vec(&read.records()[0]).unwrap();
    let changed = std::fs::read_to_string(fixture.body()).unwrap().replace("9007199254740993", "9007199254740994");
    assert!(changed.contains("9007199254740994"));
    std::fs::write(fixture.body(), changed).unwrap();
    assert_eq!(serde_json::to_vec(&read.records()[0]).unwrap(), original);
    read.reference(0).unwrap().validate_canonical_record(read.source(), &read.records()[0].record).unwrap();
    assert!(fixture.read(CompletedSnapshotBudget::default()).await.is_err());
    assert_eq!(fixture.state().await, OutboxCaptureState::Quarantined);
    assert!(read.into_cached_runtime_read().unwrap().is_valid());
}

#[tokio::test]
async fn an_unconfirmed_native_owner_cannot_gain_authority_from_a_ready_state_label() {
    let unsettled = Fixture::with_shutdown(1, false, SourceShutdown::Unconfirmed).await;
    assert_eq!(unsettled.state().await, OutboxCaptureState::Incomplete);
    let error = unsettled.read(CompletedSnapshotBudget::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::SourceUnconfirmed)));
    assert_eq!(unsettled.outbox.status(1, None).await.unwrap().entries[0].source_work, SourceWorkState::Unconfirmed);

    let fixture = Fixture::new(1, false).await;
    fixture
        .outbox
        .files
        .run(|| {
            |files| {
                files
                    .journal
                    .conn
                    .execute(
                        "UPDATE captures SET source_work=?1",
                        [super::super::journal::encode(&SourceWorkState::Unconfirmed)?],
                    )
                    .map_err(super::super::storage)?;
                Ok(())
            }
        })
        .await
        .unwrap();
    let error = fixture.read(CompletedSnapshotBudget::default()).await.unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::SourceUnconfirmed)));
    assert_eq!(fixture.outbox.status(1, None).await.unwrap().entries[0].source_work, SourceWorkState::Unconfirmed);
}

#[tokio::test]
async fn a_queued_read_obeys_its_original_deadline_and_cancellation_without_touching_artifacts() {
    let fixture = Fixture::new(1, false).await;
    tokio::time::pause();
    let locked = fixture.outbox.files.acquire().await;
    let delivery = fixture.outbox.delivery();
    let attempt = fixture.attempt.clone();
    let reading = tokio::spawn(async move {
        delivery
            .completed_snapshot(
                &attempt,
                SnapshotDataset::CachedRuntime,
                CompletedSnapshotBudget { timeout: Duration::from_millis(1), ..Default::default() },
                &CollectionCancellation::default(),
            )
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(2)).await;
    let error = reading.await.unwrap().unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut)));
    let delivery = fixture.outbox.delivery();
    let attempt = fixture.attempt.clone();
    let cancellation = CollectionCancellation::default();
    let token = cancellation.clone();
    let reading = tokio::spawn(async move {
        delivery
            .completed_snapshot(&attempt, SnapshotDataset::CachedRuntime, CompletedSnapshotBudget::default(), &token)
            .await
    });
    tokio::task::yield_now().await;
    cancellation.cancel();
    let error = reading.await.unwrap().unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    drop(locked);
    assert_eq!(fixture.state().await, OutboxCaptureState::Ready);
}

enum AfterRow {
    Cancel,
    Rewrite(PathBuf),
}
struct ObservedRead {
    read: SnapshotRead,
    after: AfterRow,
}
impl artifacts::VerificationObserver for ObservedRead {
    fn check(&self) -> Result<()> {
        self.read.check()
    }
    fn caller_refused(&self, error: &Error) -> bool {
        self.read.caller_refused(error)
    }
    fn allowance(&self, requested: usize) -> Result<usize> {
        self.read.allowance(requested)
    }
    fn charged(&mut self, bytes: usize) -> Result<()> {
        self.read.charged(bytes)
    }
    fn manifest(&mut self, manifest: &CollectionManifest) -> Result<()> {
        self.read.manifest(manifest)
    }
    fn row(&mut self, row: &ExportedRecord, ordinal: u32) -> Result<()> {
        self.read.row(row, ordinal)?;
        match &self.after {
            AfterRow::Cancel => self.read.cancellation.cancel(),
            AfterRow::Rewrite(path) => {
                let old = std::fs::read_to_string(path).unwrap();
                let changed = old.replace("9007199254740993", "9007199254740994");
                assert_ne!(old, changed);
                assert_eq!(old.len(), changed.len());
                std::fs::write(path, changed).unwrap();
            }
        }
        self.read.check()
    }
}
fn reader(attempt: DiagnosticAttemptReceipt) -> SnapshotRead {
    SnapshotRead {
        attempt,
        dataset: SnapshotDataset::CachedRuntime,
        budget: CompletedSnapshotBudget::default(),
        deadline: Instant::now() + DEFAULT_TIMEOUT,
        cancellation: CollectionCancellation::default(),
        rows: artifacts::CanonicalRows::new(CompletedSnapshotBudget::default().max_rows),
        bytes_read: 0,
    }
}

#[tokio::test]
async fn cancellation_inside_the_verification_pass_discards_provisional_rows_and_releases_the_same_actor() {
    let fixture = Fixture::new(2, false).await;
    let attempt = fixture.attempt.clone();
    let result = fixture
        .outbox
        .files
        .run(|| {
            move |files| {
                let mut observer = ObservedRead { read: reader(attempt.clone()), after: AfterRow::Cancel };
                let error = files.validated_observed(&attempt.capture.observation_id, &mut observer).err().unwrap();
                assert_eq!(
                    observer.read.rows.retained_rows(),
                    1,
                    "the pass was interrupted after genuine canonical data was read"
                );
                assert!(observer.read.bytes_read > 0);
                Ok(error)
            }
        })
        .await
        .unwrap();
    assert_eq!(result.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::Cancelled)));
    assert_eq!(fixture.state().await, OutboxCaptureState::Ready);
    assert_eq!(fixture.read(CompletedSnapshotBudget::default()).await.unwrap().records().len(), 2);
}

#[tokio::test]
async fn changed_files_after_consumption_cannot_supply_a_second_unverified_projection() {
    let fixture = Fixture::new(1, false).await;
    let original = fixture.read(CompletedSnapshotBudget::default()).await.unwrap();
    let expected = serde_json::to_vec(&original.records()[0]).unwrap();
    let expected_bytes = fixture.bytes();
    let attempt = fixture.attempt.clone();
    let body = fixture.body();
    let (rows, charged) = fixture
        .outbox
        .files
        .run(|| {
            move |files| {
                let mut observer = ObservedRead { read: reader(attempt.clone()), after: AfterRow::Rewrite(body) };
                let validated = files.validated_observed(&attempt.capture.observation_id, &mut observer)?;
                assert_eq!(validated.manifest.capture, attempt.capture);
                Ok((observer.read.rows.into_parts().0, observer.read.bytes_read))
            }
        })
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_vec(&rows[0]).unwrap(),
        expected,
        "the returned projection is the exact row whose consumed bytes passed the original manifest digest"
    );
    assert_eq!(charged, expected_bytes);
    assert!(fixture.read(CompletedSnapshotBudget::default()).await.is_err());
    assert_eq!(fixture.state().await, OutboxCaptureState::Quarantined);
}
