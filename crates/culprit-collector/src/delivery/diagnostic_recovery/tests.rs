use super::*;
use crate::{
    analytical::test_support::investigation_fixture,
    collection_files::ObservationFiles,
    delivery::{
        contract::Contract,
        evidence::PreparedRange,
        file_store::{FileOwnership, FileRecoveryStore},
        recovery::stage_and_commit,
    },
};
use std::path::Path;

struct CapturedInvestigation {
    root: tempfile::TempDir,
    store: FileRecoveryStore,
    owner: FileOwnership,
    contract: Contract,
    episode: DiagnosticEpisode,
    intent: DiagnosticInvestigationIntent,
    native: RangeManifest,
    original: DiagnosticEvidenceRef,
}

impl CapturedInvestigation {
    async fn new(owned: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let contract =
            Contract::new("pipeline".into(), "shard".into(), "file:warehouse".into(), digest(b"schema")).unwrap();
        let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
        let owner = store.acquire().await.unwrap();
        let fixture = investigation_fixture();
        let baseline = prepared(
            root.path(),
            &contract,
            fixture.baseline_record.capture.clone(),
            CollectionRequest {
                subject: CollectionSubject::Database,
                selection: CollectionSelection::Activity,
                budget: Default::default(),
            },
            None,
            fixture.baseline_source.clone(),
            vec![fixture.baseline_record.record.clone()],
        );
        let committed = stage_and_commit(&store, owner.fence(), None, &baseline).await.unwrap();
        let original = DiagnosticEvidenceRef::from_export(
            &fixture.baseline_source,
            &committed.manifest().capture_manifest_sha256,
            &fixture.baseline_record,
            0,
        )
        .unwrap();
        let episode = DiagnosticEpisode::start(
            fixture.episode.scope.clone(),
            fixture.episode.signal.clone(),
            fixture.episode.first_seen_at.clone(),
            original.clone(),
        )
        .unwrap();
        let mut intent = fixture.intent;
        intent.focus.parent = original.clone();
        intent.episode_id = episode.id.clone();
        intent.episode_revision = episode.revision.clone();
        intent.qualification.read_metrics.templates[0].client_elapsed_ms = 0.0;
        assert!(intent.is_valid(), "this structural fixture does not qualify any production recipe");
        let result = ActivityWaitFocusResult {
            focus: intent.focus.clone(),
            observed_from: "2020-01-01T12:00:01Z".into(),
            observed_until: "2020-01-01T12:00:02Z".into(),
            source: FocusSourceState::Matched,
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        };
        let page = PageWaitObservation {
            source: "page wait targets".into(),
            observed_from: result.observed_from.clone(),
            observed_until: result.observed_until.clone(),
            request: result.focus.original.clone(),
            before_check: None,
            after_check: None,
            statement_identity: None,
            plan_identity: None,
            resource: result.focus.resource.clone(),
            observed_address: None,
            resolution: PageWaitResolution::Unavailable,
            target: None,
            index_operations: None,
        };
        assert!(result.accepts_observation(&page));
        let source = CollectionDescription {
            target: intent.focus.target.clone(),
            source: "focused-pages".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedPageWait {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: result.observed_from.clone(),
                    observed_until: result.observed_until.clone(),
                    records: 1,
                    sources: vec![StateSourceCoverage::observed(page.source.clone(), 1, false)],
                },
                result: Box::new(result),
            },
        };
        let mut capture = committed.manifest().capture.clone();
        capture.observation_id = "native-focused-result".into();
        capture.revision.as_mut().unwrap().sequence = "2".into();
        let native = prepared(
            root.path(),
            &contract,
            capture,
            CollectionRequest {
                subject: CollectionSubject::Database,
                selection: CollectionSelection::FocusedPageWait { focus: Box::new(intent.focus.clone()) },
                budget: intent.budget.clone(),
            },
            owned.then(|| DiagnosticInvestigationLineage::from_intent(&intent).unwrap()),
            source,
            vec![TelemetryRecord::Operational(OperationalObservation {
                source: page.source.clone(),
                key: page.key().unwrap(),
                fact: OperationalFact::PageWait(Box::new(page)),
            })],
        );
        let native = stage_and_commit(&store, owner.fence(), Some(&committed.manifest().range_id), &native)
            .await
            .unwrap()
            .manifest()
            .clone();
        Self { root, store, owner, contract, episode, intent, native, original }
    }

    fn captured(&self) -> DiagnosticInvestigationEvent {
        let capture = capture_reference(&self.native).unwrap();
        DiagnosticInvestigationEvent {
            intent: Box::new(self.intent.clone()),
            attempt: Some(DiagnosticAttemptReceipt {
                capture: capture.capture.clone(),
                manifest_sha256: Some(capture.manifest_sha256.clone()),
            }),
            source_work: SourceWorkState::Quiescent,
            outcome: DiagnosticInvestigationOutcome::Captured { capture },
        }
    }

    fn revision(&self, event: DiagnosticInvestigationEvent) -> PreparedRange {
        let at = "2020-01-01T12:00:03Z";
        let mut next = self.episode.clone();
        next.analysis = DiagnosticAnalysisStatus::Collecting;
        let revision = DiagnosticRevision::with_investigation(&self.episode, next, event, at).unwrap().unwrap();
        let step_id = digest(revision.id.as_bytes());
        let mut checkpoint = DiagnosticCheckpointPart {
            id: String::new(),
            controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
            target: self.intent.focus.target.clone(),
            input: self.original.capture_ref(),
            part: 0,
            parts: 1,
            episodes: vec![revision.episode.clone()],
        };
        checkpoint.id = checkpoint.expected_id().unwrap();
        let source = CollectionDescription {
            target: self.intent.focus.target.clone(),
            source: DIAGNOSTICS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Diagnostics(DiagnosticCaptureDescription {
                controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
                parent: self.original.capture_ref(),
                step_id: step_id.clone(),
                analyzed_at: at.into(),
                records: 1,
                checkpoint_records: 1,
            }),
        };
        let mut capture = self.native.capture.clone();
        capture.observation_id = "derived-investigation".into();
        capture.revision.as_mut().unwrap().sequence = "3".into();
        prepared(
            self.root.path(),
            &self.contract,
            capture,
            CollectionRequest {
                subject: CollectionSubject::Database,
                selection: CollectionSelection::Diagnostics {
                    parent: self.original.capture_ref(),
                    controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
                    step_id,
                },
                budget: Default::default(),
            },
            None,
            source,
            vec![
                TelemetryRecord::Operational(OperationalObservation {
                    source: DIAGNOSTICS_SOURCE.into(),
                    key: revision.id.clone(),
                    fact: OperationalFact::DiagnosticRevision(Box::new(revision)),
                }),
                TelemetryRecord::DiagnosticCheckpoint(checkpoint),
            ],
        )
    }

    async fn commit(&self, derived: &PreparedRange) -> Result<CommittedRange> {
        stage_and_commit(&self.store, self.owner.fence(), Some(&self.native.range_id), derived).await
    }
}

fn prepared(
    root: &Path,
    contract: &Contract,
    capture: CaptureIdentity,
    request: CollectionRequest,
    owner: Option<DiagnosticInvestigationLineage>,
    source: CollectionDescription,
    rows: Vec<TelemetryRecord>,
) -> PreparedRange {
    let directory = root.join(&capture.observation_id);
    let streams = request.collection().streams();
    let mut files = ObservationFiles::create(&directory, request, capture, owner).unwrap();
    files.describe(source).unwrap();
    for (index, stream) in streams.iter().enumerate() {
        let page = rows.iter().filter(|row| row.stream() == *stream).cloned().collect::<Vec<_>>();
        if !page.is_empty() {
            files.begin_page(index).unwrap();
            assert!(files.write_page(index, &page).unwrap().is_none());
        }
        files.seal(index).unwrap();
    }
    files.confirm_source(SourceCompletion::Verified).unwrap();
    let manifest = files.finish(0).unwrap();
    assert_eq!(manifest.status, CollectionStatus::Complete);
    PreparedRange::from_validated(&directory, &manifest, contract.clone(), vec![]).unwrap()
}

#[tokio::test]
async fn captured_event_archives_only_its_exact_owned_native_result() {
    let fixture = CapturedInvestigation::new(true).await;
    let prepared = fixture.revision(fixture.captured());
    let committed = fixture.commit(&prepared).await.unwrap();
    assert_eq!(committed.manifest(), prepared.manifest());
}

#[tokio::test]
async fn captured_event_rejects_wrong_native_digest_or_absent_or_numerically_equal_foreign_owner() {
    for mismatch in ["digest", "absent-owner", "different-owner", "different-native-kind"] {
        let fixture = CapturedInvestigation::new(mismatch != "absent-owner").await;
        let mut event = fixture.captured();
        match mismatch {
            "digest" => {
                let DiagnosticInvestigationOutcome::Captured { capture } = &mut event.outcome else { unreachable!() };
                capture.manifest_sha256 = digest(b"a different original native manifest");
                event.attempt.as_mut().unwrap().manifest_sha256 = Some(capture.manifest_sha256.clone());
            }
            "different-owner" => {
                event.intent.qualification.read_metrics.templates[0].client_elapsed_ms = -0.0;
                assert_eq!(event.intent.as_ref(), &fixture.intent);
                assert_ne!(event.intent.key().unwrap(), fixture.intent.key().unwrap());
            }
            "different-native-kind" => {
                let capture = fixture.original.capture_ref();
                event.attempt = Some(DiagnosticAttemptReceipt {
                    capture: capture.capture.clone(),
                    manifest_sha256: Some(capture.manifest_sha256.clone()),
                });
                event.outcome = DiagnosticInvestigationOutcome::Captured { capture };
            }
            _ => {}
        }
        assert!(event.is_valid(), "the event is structurally valid; native archive binding must reject it");
        let prepared = fixture.revision(event);
        let error = fixture.commit(&prepared).await.err().unwrap();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{mismatch}");
        assert_eq!(fixture.store.checkpoint_head().await.unwrap(), Some(fixture.native.range_id.clone()));
    }
}

#[tokio::test]
async fn captured_event_requires_native_payload_authentication_even_after_native_manifest_commit() {
    let fixture = CapturedInvestigation::new(true).await;
    let prepared = fixture.revision(fixture.captured());
    let object = &fixture.native.batches[0].object;
    let path = fixture.root.path().join("recovery").join(object);
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    std::fs::write(path, bytes).unwrap();
    assert!(fixture.commit(&prepared).await.is_err(), "a native manifest alone cannot attest truncated payload bytes");
    assert_eq!(fixture.store.checkpoint_head().await.unwrap(), Some(fixture.native.range_id.clone()));
}
