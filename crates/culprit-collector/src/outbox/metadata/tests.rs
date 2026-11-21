use super::*;
use crate::{
    analytical::test_support::{LocalCaptureFixture, SourceAcquisitionCase},
    collection_files::ObservationFiles,
};
use culprit_core::{ErrorDetail, limits::OperationGuard};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: CollectionBudget::default(),
    }
}
async fn inspect(
    path: &Path,
    connection: &str,
    id: &str,
    budget: CompletedCaptureBudget,
) -> Result<Option<InspectedCaptureManifest>> {
    let guard = OperationGuard::new(budget.timeout).unwrap();
    Outbox::with_inspected_manifest(
        path,
        CompletedCaptureLookup { connection, observation_id: id },
        budget,
        guard.handle(),
        (),
        Ok,
    )
    .await
}
#[tokio::test]
async fn sealed_success_failure_and_native_debt_are_independent_from_ready_publication_and_next_attempt_identity() {
    for case in [SourceAcquisitionCase::Complete, SourceAcquisitionCase::Failed, SourceAcquisitionCase::Unconfirmed] {
        let mut fixture = LocalCaptureFixture::source_acquisition(case).await;
        let original = inspect(
            &fixture.directory,
            "local-evidence",
            &fixture.manifest.capture.observation_id,
            CompletedCaptureBudget::default(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(original.is_sealed());
        assert_eq!(original.manifest().capture, fixture.manifest.capture);
        assert_eq!(original.sha256(), digest(&std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap()));
        let outbox = fixture.outbox.as_mut().unwrap();
        let status = outbox.status(10, None).await.unwrap();
        assert_eq!(status.ready, u32::from(case == SourceAcquisitionCase::Complete));
        assert_eq!(status.source_debts, u32::from(case == SourceAcquisitionCase::Unconfirmed));
        if case != SourceAcquisitionCase::Complete {
            assert_eq!(status.entries[0].state, OutboxCaptureState::Incomplete);
            assert!(outbox.verify(&fixture.manifest.capture.observation_id).await.is_err());
            assert!(outbox.delivery().next_archive().await.unwrap().is_none());
        }
        if case == SourceAcquisitionCase::Failed {
            assert!(original.manifest().description.is_none());
            let next = outbox.reserve("local-evidence", &request()).await.unwrap();
            assert_ne!(next.identity(), &fixture.manifest.capture);
            next.finish().await.unwrap();
        }
        if case == SourceAcquisitionCase::Unconfirmed {
            assert!(outbox.reserve("local-evidence", &request()).await.is_err());
        }
    }
}
#[tokio::test]
async fn original_connection_and_byte_deadline_budgets_apply_without_payload_reads_or_an_exclusive_capture_owner() {
    let fixture = LocalCaptureFixture::source_acquisition(SourceAcquisitionCase::Complete).await;
    let id = &fixture.manifest.capture.observation_id;
    assert!(inspect(&fixture.directory, "foreign", id, CompletedCaptureBudget::default()).await.is_err());
    let bytes = std::fs::metadata(fixture.capture_directory.join("manifest.json")).unwrap().len();
    let exact = inspect(
        &fixture.directory,
        "local-evidence",
        id,
        CompletedCaptureBudget { max_bytes: bytes, ..Default::default() },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(exact.bytes_read(), bytes);
    assert!(
        inspect(
            &fixture.directory,
            "local-evidence",
            id,
            CompletedCaptureBudget { max_bytes: bytes - 1, ..Default::default() }
        )
        .await
        .is_err()
    );
    for file in &fixture.manifest.files {
        std::fs::remove_file(fixture.capture_directory.join(&file.file)).unwrap();
    }
    assert!(
        inspect(&fixture.directory, "local-evidence", id, CompletedCaptureBudget::default()).await.unwrap().is_some()
    );
    assert_eq!(
        Outbox::open(&fixture.directory).err().unwrap().detail,
        Some(ErrorDetail::Outbox(OutboxProblem::Locked))
    );
}
#[tokio::test]
async fn initial_absence_and_active_provisional_metadata_remain_unknown_and_cancellation_never_projects() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("outbox");
    let mut outbox = Outbox::create(&directory, Default::default()).unwrap();
    let lease = outbox.reserve("local-evidence", &request()).await.unwrap();
    let id = lease.identity().observation_id.clone();
    assert!(inspect(&directory, "local-evidence", &id, Default::default()).await.unwrap().is_none());
    let files = ObservationFiles::create(lease.directory(), request(), lease.identity().clone(), None).unwrap();
    let provisional = inspect(&directory, "local-evidence", &id, Default::default()).await.unwrap().unwrap();
    assert!(!provisional.is_sealed());
    assert!(provisional.manifest().finished_at.is_none());
    let guard = OperationGuard::new(Duration::from_secs(5)).unwrap();
    let operation = guard.handle();
    drop(guard);
    let projected = Arc::new(AtomicBool::new(false));
    let called = projected.clone();
    assert!(
        Outbox::with_inspected_manifest(
            &directory,
            CompletedCaptureLookup { connection: "local-evidence", observation_id: &id },
            Default::default(),
            operation,
            (),
            move |_| {
                called.store(true, Ordering::SeqCst);
                Ok(())
            }
        )
        .await
        .is_err()
    );
    assert!(!projected.load(Ordering::SeqCst));
    drop(files);
    lease.finish().await.unwrap();
}
#[tokio::test]
async fn terminal_failure_seal_refuses_changed_bytes_and_cannot_be_reused_to_finish_the_same_revision() {
    let mut fixture = LocalCaptureFixture::source_acquisition(SourceAcquisitionCase::Failed).await;
    let id = fixture.manifest.capture.observation_id.clone();
    let directory = fixture.capture_directory.clone();
    assert!(
        ObservationFiles::create(&directory, fixture.manifest.request.clone(), fixture.manifest.capture.clone(), None)
            .is_err(),
        "an existing sealed revision cannot reopen its writer directory"
    );
    let outbox = fixture.outbox.as_mut().unwrap();
    let duplicate = outbox
        .files
        .run(|| {
            move |files| {
                let entry = files.journal.capture(&id)?;
                files.finish(super::super::Reservation { entry, directory })
            }
        })
        .await;
    assert!(duplicate.is_err());
    let mut changed = fixture.manifest.clone();
    changed.source_queries += 1;
    std::fs::write(fixture.capture_directory.join("manifest.json"), serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        inspect(&fixture.directory, "local-evidence", &fixture.manifest.capture.observation_id, Default::default())
            .await
            .is_err()
    );
    assert_eq!(outbox.status(10, None).await.unwrap().entries[0].state, OutboxCaptureState::Incomplete);
}
#[tokio::test]
async fn sealing_and_journal_route_changes_between_original_read_and_recheck_refuse_without_retry() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, Default::default()).unwrap();
    let lease = outbox.reserve("local-evidence", &request()).await.unwrap();
    let id = lease.identity().observation_id.clone();
    let directory = lease.directory().to_owned();
    let files = ObservationFiles::create(&directory, request(), lease.identity().clone(), None).unwrap();
    let identity = read_identity(&path).unwrap();
    let journal = journal::Journal::open(&path.join("journal.sqlite3"), &identity.id, false).unwrap();
    let before = journal.capture(&id).unwrap();
    let seal = manifest_seal(&journal, &id).unwrap();
    let guard = OperationGuard::new(Duration::from_secs(5)).unwrap();
    let mut read = CompletedCaptureRead::source_overhead(
        CompletedCaptureLookup { connection: "local-evidence", observation_id: &id },
        Default::default(),
        guard.handle(),
    )
    .unwrap();
    let original = attempts::read_manifest_observed(&path, &journal, &before, &mut read).unwrap().unwrap();
    assert!(!original.sealed);
    files.finish(0).unwrap();
    lease.finish().await.unwrap();
    assert!(manifest_seal(&journal, &id).unwrap().is_some());
    assert!(recheck(&journal, &before, "local-evidence", seal.as_deref()).is_err());
    let before = journal.capture(&id).unwrap();
    let seal = manifest_seal(&journal, &id).unwrap();
    let writer = rusqlite::Connection::open(path.join("journal.sqlite3")).unwrap();
    writer.execute("UPDATE captures SET connection='foreign' WHERE observation_id=?1", [&id]).unwrap();
    assert!(recheck(&journal, &before, "local-evidence", seal.as_deref()).is_err());
}
