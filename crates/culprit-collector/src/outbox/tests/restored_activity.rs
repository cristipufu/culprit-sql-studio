use super::*;
use crate::delivery::{activity_recovery::*, recovery::*};

fn observation() -> ActivityObservation {
    serde_json::from_value(serde_json::json!({
        "key":{"session":{"id":"61","generation":"login-1"},"request":{"id":"0","generation":"request-1"}},
        "idle":false,"scope":"request-database","state":"running","open_transactions":0,"elapsed_ms":"120000",
        "session_counters":{},"request_counters":{},"statement":{"state":"unavailable","reason":"not-collected"}
    }))
    .unwrap()
}
async fn checkpoint(store: &dyn RecoveryStore) -> RecoveredActivityCheckpoint {
    let heads = store.activity_heads(None, 128).await.unwrap();
    assert_eq!(heads.heads.len(), 1);
    load_activity_checkpoint(store, &heads.heads[0], ActivityRestoreBudget::default()).await.unwrap()
}

#[tokio::test]
async fn volume_loss_restores_incidents_by_native_identity_and_a_complete_empty_successor_clears_them() {
    let root = tempfile::tempdir().unwrap();
    let (store, owner, contract, mut original) = super::eviction::remote_box(root.path()).await;
    let first = super::eviction::activity(&mut original, "2020-01-01T06:00:00Z", Some(observation())).await;
    let prepared = original.delivery().prepare(&first.capture.observation_id, contract.clone(), vec![]).await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    let recovered = checkpoint(&store).await;
    let incident = recovered.states()[0].clone();
    let destination = root.path().join("replacement");
    let mut outbox = Outbox::create_remote(&destination, OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    outbox.restore_activity(recovered).await.unwrap();
    assert!(outbox.status(1, None).await.unwrap().charged_bytes > 0);
    drop(outbox);
    let mut outbox = Outbox::open(&destination).unwrap();
    outbox.recover().await.unwrap();
    // The connection alias did not survive the lost volume. The native database identity did.
    outbox.begin_remote_epoch(&Fence::issued(2, "new-owner".into(), store.id().into()).unwrap(), 1).await.unwrap();
    let request = CollectionRequest {
        selection: CollectionSelection::Activity { policy: ActivityPolicy::default() },
        budget: CollectionBudget::default(),
    };
    let lease = outbox.reserve("renamed-connection", &request).await.unwrap();
    let source = first.description.unwrap();
    let baseline = lease.baseline(&source).await.unwrap().unwrap();
    assert_eq!(baseline.matching(&source), [incident]);
    let mut restarted = source.clone();
    let SourceObservation::Activity(snapshot) = &mut restarted.observation else { unreachable!() };
    snapshot.epoch = "native-restart".into();
    assert!(baseline.matching(&restarted).is_empty());
    drop(lease);
    drop(outbox);
    let second = super::eviction::activity(&mut original, "2020-01-01T06:01:00Z", None).await;
    let prepared = original.delivery().prepare(&second.capture.observation_id, contract, vec![]).await.unwrap();
    stage_and_commit(&store, owner.fence(), Some(&committed.manifest().range_id), &prepared).await.unwrap();
    let mut outbox = Outbox::open(&destination).unwrap();
    outbox.recover().await.unwrap();
    outbox.restore_activity(checkpoint(&store).await).await.unwrap();
    let second_source = second.description.unwrap();
    let baseline = outbox.files.run(|| move |files| files.restored_baseline(&second_source)).await.unwrap().unwrap();
    assert!(baseline.states.is_empty());
    assert_eq!(baseline.capture, second.capture);
}

#[tokio::test]
async fn interrupted_checkpoint_installation_keeps_the_previous_head_and_reclaims_its_reservation() {
    let root = tempfile::tempdir().unwrap();
    let (store, owner, contract, mut original) = super::eviction::remote_box(root.path()).await;
    let first = super::eviction::activity(&mut original, "2020-01-01T06:00:00Z", Some(observation())).await;
    let prepared = original.delivery().prepare(&first.capture.observation_id, contract.clone(), vec![]).await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    let destination = root.path().join("replacement");
    let mut outbox = Outbox::create_remote(&destination, OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    outbox.restore_activity(checkpoint(&store).await).await.unwrap();
    let before = outbox.status(1, None).await.unwrap().charged_bytes;
    let second = super::eviction::activity(&mut original, "2020-01-01T06:01:00Z", None).await;
    let prepared = original.delivery().prepare(&second.capture.observation_id, contract, vec![]).await.unwrap();
    stage_and_commit(&store, owner.fence(), Some(&committed.manifest().range_id), &prepared).await.unwrap();
    journal_fault(
        &outbox,
        "CREATE TRIGGER fail_head BEFORE UPDATE ON baseline_heads BEGIN SELECT RAISE(FAIL,'test-stop'); END",
    )
    .await;
    assert!(outbox.restore_activity(checkpoint(&store).await).await.is_err());
    assert!(outbox.status(1, None).await.unwrap().charged_bytes > before);
    journal_fault(&outbox, "DROP TRIGGER fail_head").await;
    drop(outbox);
    let mut outbox = Outbox::open(&destination).unwrap();
    outbox.recover().await.unwrap();
    assert_eq!(outbox.status(1, None).await.unwrap().charged_bytes, before);
    let source = first.description.unwrap();
    assert_eq!(
        outbox.files.run(|| move |files| files.restored_baseline(&source)).await.unwrap().unwrap().states.len(),
        1
    );
    outbox.restore_activity(checkpoint(&store).await).await.unwrap();
    assert_eq!(std::fs::read_dir(destination.join("baselines")).unwrap().count(), 1);
}

#[tokio::test]
async fn checkpoint_budget_identity_and_complete_native_eof_are_required_before_installation() {
    let root = tempfile::tempdir().unwrap();
    let (store, owner, contract, mut original) = super::eviction::remote_box(root.path()).await;
    let first = super::eviction::activity(&mut original, "2020-01-01T06:00:00Z", Some(observation())).await;
    let prepared = original.delivery().prepare(&first.capture.observation_id, contract, vec![]).await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    let head = store.activity_heads(None, 1).await.unwrap().heads.remove(0);
    assert_eq!(
        load_activity_checkpoint(&store, &head, ActivityRestoreBudget { max_states: 1, max_bytes: 1 })
            .await
            .err()
            .unwrap()
            .detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity))
    );
    let mut wrong = head.clone();
    wrong.manifest_digest = digest(b"wrong-manifest");
    assert!(load_activity_checkpoint(&store, &wrong, ActivityRestoreBudget::default()).await.is_err());
    let native =
        committed.manifest().batches.iter().find(|b| b.stream == TelemetryStream::ActivityCheckpoints).unwrap();
    let path = root.path().join("recovery").join(&native.object);
    let body = std::fs::read(&path).unwrap();
    let mut changed = body.clone();
    // Preserve object length and valid JSON; the final checksum still has to match.
    let last = changed.len() - 1;
    changed[last] = b' ';
    std::fs::write(&path, &changed).unwrap();
    assert!(load_activity_checkpoint(&store, &head, ActivityRestoreBudget::default()).await.is_err());
    std::fs::write(&path, &body).unwrap();
    let destination = root.path().join("small-outbox");
    let mut outbox =
        Outbox::create_remote(&destination, OutboxLimits { max_bytes: 1024 * 1024, max_captures: 1 }, store.id())
            .unwrap();
    outbox.recover().await.unwrap();
    outbox.restore_activity(checkpoint(&store).await).await.unwrap();
    // Corruption cannot be treated as an empty checkpoint or a successful recovery.
    let path = std::fs::read_dir(destination.join("baselines")).unwrap().next().unwrap().unwrap().path();
    std::fs::write(path, b"[]\n").unwrap();
    let source = first.description.unwrap();
    assert!(outbox.files.run(|| move |files| files.restored_baseline(&source)).await.is_err());
}
