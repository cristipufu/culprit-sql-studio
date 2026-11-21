use super::*;
use crate::analytical::test_support::{PageWriterCase, page_writer_fixture};

struct WriterArchive {
    root: tempfile::TempDir,
    store: FileRecoveryStore,
    native: RangeManifest,
    original: RangeManifest,
    page: RangeManifest,
    episode: DiagnosticEpisode,
}

impl WriterArchive {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let contract = Contract::new(
            "pipeline".into(),
            "shard".into(),
            "file:writer-proof".into(),
            digest(b"writer-proof-schema"),
        )
        .unwrap();
        let store = FileRecoveryStore::create(&root.path().join("recovery"), contract.clone()).unwrap();
        let owner = store.acquire().await.unwrap();
        let mut fixture = page_writer_fixture(PageWriterCase::ReportedBlockingSession);
        let mut focus = match &fixture.source.observation {
            SourceObservation::PageWaitWriters { focus, .. } => focus.as_ref().clone(),
            _ => unreachable!(),
        };
        let original_capture = focus.waiter.participant.parent.capture.clone();
        let original = prepared(
            root.path(),
            &contract,
            original_capture,
            request(CollectionSelection::Activity),
            None,
            fixture.activity_source.clone(),
            fixture.activity_rows.clone(),
        );
        let original = stage_and_commit(&store, owner.fence(), None, &original).await.unwrap();
        let original_manifest = original.manifest();
        let original_refs = fixture
            .activity_rows
            .iter()
            .enumerate()
            .map(|(ordinal, record)| {
                let exported =
                    ExportedRecord::new(&fixture.activity_source, &original_manifest.capture, record.clone()).unwrap();
                DiagnosticEvidenceRef::from_export(
                    &fixture.activity_source,
                    &original_manifest.capture_manifest_sha256,
                    &exported,
                    ordinal as u32,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        focus.waiter.participant.parent = original_refs[0].clone();
        for (candidate, reference) in focus.candidates.iter_mut().zip(&original_refs[1..]) {
            candidate.parent = reference.clone();
        }
        let page_capture = focus.page.capture.clone();
        let page = prepared(
            root.path(),
            &contract,
            page_capture,
            request(CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets }),
            None,
            fixture.page_source.clone(),
            fixture.page_rows.clone(),
        );
        let page = stage_and_commit(&store, owner.fence(), Some(&original_manifest.range_id), &page).await.unwrap();
        let page_row =
            ExportedRecord::new(&fixture.page_source, &page.manifest().capture, fixture.page_rows[0].clone()).unwrap();
        focus.page = DiagnosticEvidenceRef::from_export(
            &fixture.page_source,
            &page.manifest().capture_manifest_sha256,
            &page_row,
            0,
        )
        .unwrap();
        let TelemetryRecord::Operational(row) = &mut fixture.records[0] else { unreachable!() };
        let OperationalFact::PageWaitWriters(point) = &mut row.fact else { unreachable!() };
        point.focus_key = focus.key().unwrap();
        for (current, original) in point.candidates.iter_mut().zip(&focus.candidates) {
            current.parent = original.parent.clone();
            if let Some(proof) = current.binding.take() {
                current.binding = Some(
                    MssqlPageWritePlanProof::new(
                        &focus.target,
                        current,
                        proof.native_plan_sha256,
                        proof.native_statement_sha256,
                        proof.accesses,
                    )
                    .unwrap(),
                );
            }
        }
        row.key = point.key().unwrap();
        let SourceObservation::PageWaitWriters { focus: declared, .. } = &mut fixture.source.observation else {
            unreachable!()
        };
        **declared = focus.clone();
        let mut native_capture = page.manifest().capture.clone();
        native_capture.observation_id = "writer-native-point".into();
        native_capture.revision.as_mut().unwrap().sequence = "820003".into();
        let native = prepared(
            root.path(),
            &contract,
            native_capture,
            request(CollectionSelection::PageWaitWriters { focus: Box::new(focus.clone()) }),
            None,
            fixture.source.clone(),
            fixture.records.clone(),
        );
        let native = stage_and_commit(&store, owner.fence(), Some(&page.manifest().range_id), &native).await.unwrap();
        let native_row =
            ExportedRecord::new(&fixture.source, &native.manifest().capture, fixture.records[0].clone()).unwrap();
        let reference = DiagnosticEvidenceRef::from_export(
            &fixture.source,
            &native.manifest().capture_manifest_sha256,
            &native_row,
            0,
        )
        .unwrap();
        let mut episode = DiagnosticEpisode::start(
            DiagnosticScope::from_source(&fixture.activity_source).unwrap(),
            DiagnosticSignal::PageWait {
                key: focus.waiter.participant.original.key.clone(),
                statement_identity: Some(focus.waiter.participant.statement_identity.clone()),
                resource: focus.waiter.resource.clone(),
            },
            focus.waiter.participant.original_observed_at.clone(),
            original_refs[0].clone(),
        )
        .unwrap();
        episode.analysis = DiagnosticAnalysisStatus::Analyzed;
        episode.report = Some(DiagnosticReport::PageWaitWriters {
            observation: reference,
            page: Box::new(focus.page.clone()),
            candidates: original_refs[1..].to_vec(),
            blocking_contexts: original_refs[1..].to_vec(),
        });
        assert!(episode.is_valid());
        Self {
            root,
            store,
            native: native.manifest().clone(),
            original: original_manifest.clone(),
            page: page.manifest().clone(),
            episode,
        }
    }
}

fn request(selection: CollectionSelection) -> CollectionRequest {
    CollectionRequest { subject: CollectionSubject::Database, selection, budget: Default::default() }
}

#[tokio::test]
async fn archive_authenticates_the_writer_roster_and_every_original_role_without_losing_mars_contexts() {
    let fixture = WriterArchive::new().await;
    authenticate_report_participants(&fixture.store, &fixture.native, &fixture.episode, &mut BTreeMap::new())
        .await
        .unwrap();
    for role in ["page", "candidate", "context"] {
        let mut episode = fixture.episode.clone();
        let Some(DiagnosticReport::PageWaitWriters { page, candidates, blocking_contexts, .. }) = &mut episode.report
        else {
            unreachable!()
        };
        match role {
            "page" => page.payload_sha256 = digest(b"foreign mapped page body"),
            "candidate" => {
                candidates.pop();
            }
            "context" => {
                blocking_contexts.pop();
            }
            _ => unreachable!(),
        }
        assert!(episode.is_valid(), "shape-only consistency does not authenticate {role}");
        let error = authenticate_report_participants(&fixture.store, &fixture.native, &episode, &mut BTreeMap::new())
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{role}");
    }
}

#[tokio::test]
async fn archived_receipt_membership_does_not_authorize_a_missing_or_corrupt_original_frame_or_page_body() {
    for missing_page in [false, true] {
        let fixture = WriterArchive::new().await;
        let manifest = if missing_page { &fixture.page } else { &fixture.original };
        let batch = &manifest.batches[0];
        let path = fixture.root.path().join("recovery").join(&batch.object);
        let mut bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.pop(), Some(b'\n'));
        std::fs::write(path, bytes).unwrap();
        assert!(
            authenticate_report_participants(&fixture.store, &fixture.native, &fixture.episode, &mut BTreeMap::new())
                .await
                .is_err()
        );
    }
}
