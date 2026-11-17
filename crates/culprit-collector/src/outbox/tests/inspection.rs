use super::*;

#[tokio::test]
async fn metadata_inspection_keeps_the_active_owner_and_unfinished_or_unconfirmed_native_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let mut lease = outbox.reserve("fixture", &request()).await.unwrap();
    let identity = lease.identity().clone();
    assert!(Outbox::inspect_manifest(&path, &identity.observation_id).await.unwrap().is_none());
    let mut files = ObservationFiles::create(lease.directory(), request(), identity.clone(), None).unwrap();
    lease.source_started().await.unwrap();
    let recorded = Outbox::inspect_manifest(&path, &identity.observation_id).await.unwrap().unwrap();
    assert_eq!(recorded.capture, identity);
    assert_eq!(recorded.status, CollectionStatus::Incomplete(CollectionStop::Pending));
    assert!(recorded.finished_at.is_none());
    assert!(recorded.description.is_none());
    assert!(recorded.read_metrics.is_none());
    assert_eq!(kind(&Outbox::open(&path).err().unwrap()), OutboxProblem::Locked);
    files.invalidate(CollectionStop::CancellationUnconfirmed);
    files.finish(0).unwrap();
    lease.source_closed(SourceShutdown::Unconfirmed).await.unwrap();
    lease.finish().await.unwrap();
    let before = outbox.status(10, None).await.unwrap();
    let recorded = Outbox::inspect_manifest(&path, &identity.observation_id).await.unwrap().unwrap();
    assert_eq!(recorded.status, CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed));
    assert!(recorded.finished_at.is_some());
    assert_eq!(recorded.source_queries, 0);
    let after = outbox.status(10, None).await.unwrap();
    assert_eq!((before.source_debts, after.source_debts), (1, 1));
    assert_eq!(after.entries[0].source_work, SourceWorkState::Unconfirmed);
    assert_eq!(after.entries[0].state, OutboxCaptureState::Incomplete);
}

#[tokio::test]
async fn metadata_does_not_open_result_payloads_or_replace_the_sealed_manifest_receipt() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let original = complete(&mut outbox).await;
    let directory = path.join("captures").join(&original.capture.observation_id);
    std::fs::remove_file(directory.join(&original.files[0].file)).unwrap();
    let recorded = Outbox::inspect_manifest(&path, &original.capture.observation_id).await.unwrap().unwrap();
    assert_eq!(recorded.capture, original.capture);
    assert_eq!(recorded.status, CollectionStatus::Complete);
    let mut forged = recorded;
    forged.source_queries += 1;
    std::fs::write(directory.join("manifest.json"), serde_json::to_vec(&forged).unwrap()).unwrap();
    assert_eq!(
        kind(&Outbox::inspect_manifest(&path, &original.capture.observation_id).await.unwrap_err()),
        OutboxProblem::Integrity
    );
    assert_eq!(
        outbox.status(10, None).await.unwrap().ready,
        1,
        "read-only inspection does not quarantine or repair state"
    );
}

#[tokio::test]
async fn local_metadata_rejects_foreign_ids_changed_request_noncanonical_and_oversized_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let lease = outbox.reserve("fixture", &request()).await.unwrap();
    let identity = lease.identity().clone();
    let directory = lease.directory().to_owned();
    let files = ObservationFiles::create(&directory, request(), identity.clone(), None).unwrap();
    let bytes = std::fs::read(directory.join("manifest.json")).unwrap();
    for field in ["capture", "request", "unknown"] {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        match field {
            "capture" => value["capture"]["observation_id"] = serde_json::json!("f".repeat(32)),
            "request" => value["request"]["budget"]["max_queries"] = serde_json::json!(1),
            _ => value["unrecognized-private-field"] = serde_json::json!(true),
        }
        std::fs::write(directory.join("manifest.json"), serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            kind(&Outbox::inspect_manifest(&path, &identity.observation_id).await.unwrap_err()),
            OutboxProblem::Integrity,
            "{field}"
        );
    }
    std::fs::write(directory.join("manifest.json"), vec![b' '; MANIFEST_LIMIT + 1]).unwrap();
    assert_eq!(
        kind(&Outbox::inspect_manifest(&path, &identity.observation_id).await.unwrap_err()),
        OutboxProblem::Integrity
    );
    for id in ["../outside", "/absolute", "", "not-an-owned-capture"] {
        assert_eq!(kind(&Outbox::inspect_manifest(&path, id).await.unwrap_err()), OutboxProblem::MissingCapture);
    }
    std::fs::remove_file(directory.join("manifest.json")).unwrap();
    assert_eq!(
        kind(&Outbox::inspect_manifest(&path, &identity.observation_id).await.unwrap_err()),
        OutboxProblem::Integrity,
        "existing stream artifacts cannot be mistaken for an allocation before initial publication"
    );
    std::fs::write(directory.join("manifest.json"), bytes).unwrap();
    drop(files);
    lease.finish().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn metadata_refuses_symlinked_manifests_and_capture_directories() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let original = complete(&mut outbox).await;
    let directory = path.join("captures").join(&original.capture.observation_id);
    let manifest = directory.join("manifest.json");
    let outside = root.path().join("outside.json");
    std::fs::rename(&manifest, &outside).unwrap();
    symlink(&outside, &manifest).unwrap();
    assert!(Outbox::inspect_manifest(&path, &original.capture.observation_id).await.is_err());
    std::fs::remove_file(&manifest).unwrap();
    std::fs::rename(&outside, &manifest).unwrap();
    let moved = root.path().join("moved-capture");
    std::fs::rename(&directory, &moved).unwrap();
    symlink(&moved, &directory).unwrap();
    assert_eq!(
        kind(&Outbox::inspect_manifest(&path, &original.capture.observation_id).await.unwrap_err()),
        OutboxProblem::Integrity
    );
}
