use super::*;

#[tokio::test]
async fn allocated_attempt_without_a_manifest_keeps_identity_without_inventing_a_digest_or_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let mut outbox = Outbox::create(&path, OutboxLimits::default()).unwrap();
    let lease = outbox.reserve("fixture", &request()).await.unwrap();
    let identity = lease.identity().clone();
    lease.finish().await.unwrap();
    let receipt = outbox.attempt_receipt(&identity, None).await.unwrap();
    assert_eq!(receipt.capture, identity);
    assert_eq!(receipt.manifest_sha256, None);
    let status = outbox.status(10, None).await.unwrap();
    assert_eq!(status.ready, 0);
    assert_eq!(status.entries[0].source_work, SourceWorkState::NotStarted);
    assert_eq!(status.entries[0].state, OutboxCaptureState::Incomplete);
    assert!(outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());
    let mut foreign = identity.clone();
    foreign.revision.as_mut().unwrap().sequence = "999".into();
    assert_eq!(kind(&outbox.attempt_receipt(&foreign, None).await.unwrap_err()), OutboxProblem::Integrity);
    drop(outbox);
    let outbox = Outbox::open(&path).unwrap();
    assert_eq!(outbox.attempt_receipt(&identity, None).await.unwrap(), receipt);
}

#[tokio::test]
async fn incomplete_attempt_digest_attests_actual_bytes_and_never_clears_unconfirmed_work() {
    let root = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    let mut lease = outbox.reserve("fixture", &request()).await.unwrap();
    let identity = lease.identity().clone();
    let directory = lease.directory().to_owned();
    let files = ObservationFiles::create(&directory, request(), identity.clone(), None).unwrap();
    lease.source_started().await.unwrap();
    let manifest = files.finish(0).unwrap();
    lease.source_closed(SourceShutdown::Unconfirmed).await.unwrap();
    lease.finish().await.unwrap();
    assert!(matches!(manifest.status, CollectionStatus::Incomplete(_)));
    let bytes = std::fs::read(directory.join("manifest.json")).unwrap();
    let receipt = outbox.attempt_receipt(&identity, None).await.unwrap();
    assert_eq!(receipt.manifest_sha256, Some(digest(&bytes)));
    let status = outbox.status(10, None).await.unwrap();
    assert_eq!((status.source_debts, status.ready), (1, 0));
    assert_eq!(status.entries[0].source_work, SourceWorkState::Unconfirmed);
    assert!(outbox.reserve("fixture", &request()).await.is_err());
    assert!(outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap().is_none());

    std::fs::remove_file(directory.join("manifest.json")).unwrap();
    assert_eq!(
        kind(&outbox.attempt_receipt(&identity, None).await.unwrap_err()),
        OutboxProblem::Integrity,
        "native stream artifacts prove the initial manifest existed; its loss cannot mean not yet created"
    );
    for bytes in [b"{".as_slice(), &vec![b' '; MANIFEST_LIMIT + 1]] {
        std::fs::write(directory.join("manifest.json"), bytes).unwrap();
        assert_eq!(kind(&outbox.attempt_receipt(&identity, None).await.unwrap_err()), OutboxProblem::Integrity);
    }
    assert_eq!(outbox.status(10, None).await.unwrap().source_debts, 1);
}

#[tokio::test]
async fn sealed_attempt_rejects_changed_bytes_wrong_request_foreign_capture_and_missing_manifest() {
    let root = tempfile::tempdir().unwrap();
    let mut outbox = Outbox::create(&root.path().join("outbox"), OutboxLimits::default()).unwrap();
    let manifest = complete(&mut outbox).await;
    let identity = manifest.capture.clone();
    let manifest_path = root.path().join("outbox/captures").join(&identity.observation_id).join("manifest.json");
    let bytes = std::fs::read(&manifest_path).unwrap();
    assert_eq!(outbox.attempt_receipt(&identity, None).await.unwrap().manifest_sha256, Some(digest(&bytes)));
    for changed in ["bytes", "request", "capture"] {
        let mut forged = manifest.clone();
        match changed {
            "bytes" => forged.started_at = "2019-01-01T00:00:00Z".into(),
            "request" => forged.request.budget.max_queries += 1,
            "capture" => forged.capture.revision.as_mut().unwrap().sequence = "999".into(),
            _ => unreachable!(),
        }
        std::fs::write(&manifest_path, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert_eq!(kind(&outbox.attempt_receipt(&identity, None).await.unwrap_err()), OutboxProblem::Integrity);
    }
    std::fs::remove_file(&manifest_path).unwrap();
    assert_eq!(kind(&outbox.attempt_receipt(&identity, None).await.unwrap_err()), OutboxProblem::Integrity);
    assert_eq!(outbox.status(10, None).await.unwrap().ready, 1, "receipt inspection does not mutate capture state");
}
