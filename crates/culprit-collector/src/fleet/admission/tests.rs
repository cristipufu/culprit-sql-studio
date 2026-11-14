use super::*;
use crate::fleet::FleetCaptureOutcome;
use std::sync::atomic::AtomicU32;

struct Backend {
    source_work: SourceWorkState,
    manifest_unconfirmed: bool,
    error_unconfirmed: bool,
    calls: AtomicU32,
}
#[async_trait::async_trait]
impl FleetBackend for Backend {
    fn validate_target(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn discover(
        &self,
        _: &str,
        _: &IntervalDiscoveryRequest,
        _: FleetSourceAdmission,
    ) -> Result<IntervalDiscoveryPage> {
        panic!("capture admission must not discover a source")
    }
    async fn capture(
        &self,
        _: &str,
        request: CollectionRequest,
        _: &mut Outbox,
        _: &CollectionCancellation,
        _: FleetSourceAdmission,
    ) -> FleetCaptureOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = if self.manifest_unconfirmed {
            Ok(CollectionManifest {
                format_version: EXPORT_FORMAT_VERSION,
                capture: CaptureIdentity { observation_id: "unconfirmed-fixture".into(), revision: None },
                started_at: "2020-01-01T12:00:00Z".into(),
                finished_at: Some("2020-01-01T12:00:01Z".into()),
                request,
                description: None,
                status: CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed),
                source_queries: 1,
                read_metrics: None,
                directory_synced: true,
                files: vec![],
                references: ReferenceCoverage::default(),
            })
        } else if self.error_unconfirmed {
            Err(CollectionStop::CancellationUnconfirmed.error())
        } else {
            Err(CollectionStop::Pressure.error())
        };
        FleetCaptureOutcome { result, source_work: self.source_work }
    }
}
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
        budget: CollectionBudget::default(),
    }
}

#[tokio::test]
async fn only_the_returned_owners_closure_proof_settles_a_failed_capture() {
    let directory = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&directory.path().join("outbox"), Default::default()).unwrap();
    for source_work in
        [SourceWorkState::NotStarted, SourceWorkState::Quiescent, SourceWorkState::Active, SourceWorkState::Unconfirmed]
    {
        let backend =
            Backend { source_work, manifest_unconfirmed: false, error_unconfirmed: false, calls: AtomicU32::new(0) };
        let admitted = AdmittedBackend::new(&backend, Instant::now());
        let error =
            admitted.capture("source", request(), &mut outbox, &CollectionCancellation::default()).await.unwrap_err();
        let (expected, stopped) = match source_work {
            SourceWorkState::NotStarted | SourceWorkState::Quiescent => (source_work, CollectionStop::Pressure),
            SourceWorkState::Active | SourceWorkState::Unconfirmed => {
                (SourceWorkState::Unconfirmed, CollectionStop::CancellationUnconfirmed)
            }
        };
        assert_eq!(admitted.source_work(), expected);
        assert_eq!(CollectionStop::from_error(&error), stopped);
        assert!(admitted.capture("source", request(), &mut outbox, &CollectionCancellation::default()).await.is_err());
        assert_eq!(admitted.source_work(), expected, "a rejected duplicate cannot overwrite native settlement");
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn expired_dispatch_has_explicit_never_started_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&directory.path().join("outbox"), Default::default()).unwrap();
    let backend = Backend {
        source_work: SourceWorkState::Quiescent,
        manifest_unconfirmed: false,
        error_unconfirmed: false,
        calls: AtomicU32::new(0),
    };
    let admitted = AdmittedBackend::new(&backend, Instant::now());
    tokio::time::advance(ADMISSION_START_WINDOW).await;
    let error =
        admitted.capture("source", request(), &mut outbox, &CollectionCancellation::default()).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
    assert_eq!(admitted.source_work(), SourceWorkState::NotStarted);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unconfirmed_native_evidence_cannot_be_overridden_by_a_claimed_quiet_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&directory.path().join("outbox"), Default::default()).unwrap();
    for (manifest_unconfirmed, error_unconfirmed) in [(true, false), (false, true)] {
        let backend = Backend {
            source_work: SourceWorkState::Quiescent,
            manifest_unconfirmed,
            error_unconfirmed,
            calls: AtomicU32::new(0),
        };
        let admitted = AdmittedBackend::new(&backend, Instant::now());
        let error =
            admitted.capture("source", request(), &mut outbox, &CollectionCancellation::default()).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
        assert_eq!(admitted.source_work(), SourceWorkState::Unconfirmed);
    }
}
