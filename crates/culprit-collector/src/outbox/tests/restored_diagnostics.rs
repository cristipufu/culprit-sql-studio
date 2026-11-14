use super::*;
use crate::delivery::{diagnostic_recovery::*, recovery::*};

fn observation() -> ActivityObservation {
    serde_json::from_value(serde_json::json!({"key":{"session":{"id":"61","generation":"login-1"},"request":{"id":"0","generation":"request-1"}},"blocking_status":"known","idle":false,"scope":"request-database","state":"running","open_transactions":0,"elapsed_ms":"120000","session_counters":{},"request_counters":{},"statement":{"state":"unavailable","reason":"not-collected"}})).unwrap()
}
pub(super) fn pending_update(input: &DiagnosticInput, step: &str) -> DiagnosticUpdate {
    let rows = input
        .rows
        .iter()
        .filter_map(|row| match &row.row.record {
            TelemetryRecord::Activity(observation) => {
                Some(DiagnosticActivityRow { reference: &row.reference, observation })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let update = observe_diagnostic_activity(
        &input.source,
        &input.parent,
        &rows,
        input.checkpoint.as_ref(),
        &ActivityPolicy::default(),
        "2020-01-01T06:03:00Z",
    )
    .unwrap();
    DiagnosticUpdate {
        step_id: digest(step.as_bytes()),
        analyzed_at: "2020-01-01T06:03:00Z".into(),
        revisions: update.revisions,
        checkpoint: update.checkpoint,
    }
}
async fn archive_input_and_update(
    outbox: &mut Outbox,
    store: &dyn RecoveryStore,
    fence: &Fence,
    contract: &crate::delivery::contract::Contract,
) -> CommittedRange {
    let input = outbox.next_diagnostic_input(1).await.unwrap().unwrap();
    let prepared =
        outbox.delivery().prepare(&input.parent.capture.observation_id, contract.clone(), vec![]).await.unwrap();
    let head = store.checkpoint_head().await.unwrap();
    stage_and_commit(store, fence, head.as_deref(), &prepared).await.unwrap();
    let derived = outbox
        .commit_diagnostics(input.clone(), pending_update(&input, &input.parent.capture.observation_id))
        .await
        .unwrap();
    let prepared = outbox.delivery().prepare(&derived.observation_id, contract.clone(), vec![]).await.unwrap();
    let head = store.checkpoint_head().await.unwrap();
    stage_and_commit(store, fence, head.as_deref(), &prepared).await.unwrap()
}
async fn checkpoint(store: &dyn RecoveryStore) -> RecoveredDiagnosticCheckpoint {
    let heads = store.diagnostic_heads(None, 128).await.unwrap();
    assert_eq!(heads.heads.len(), 1);
    load_diagnostic_checkpoint(store, &heads.heads[0]).await.unwrap()
}
#[tokio::test]
async fn volume_loss_restores_native_episode_history_and_a_complete_empty_successor_preserves_owed_analysis() {
    let root = tempfile::tempdir().unwrap();
    let (store, owner, contract, mut original) = super::eviction::remote_box(root.path()).await;
    super::eviction::activity(&mut original, "2020-01-01T06:00:00Z", Some(observation())).await;
    archive_input_and_update(&mut original, &store, owner.fence(), &contract).await;
    let first = checkpoint(&store).await;
    let prior = first.checkpoint().episodes[0].clone();
    let destination = root.path().join("replacement");
    let mut outbox = Outbox::create_remote(&destination, OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    outbox.restore_diagnostics(first).await.unwrap();
    drop(outbox);
    let mut outbox = Outbox::open(&destination).unwrap();
    outbox.recover().await.unwrap();
    let restored = outbox.diagnostic_checkpoints(1, None, 10).await.unwrap();
    assert_eq!(restored.checkpoints[0].episodes[0], prior);
    super::eviction::activity(&mut original, "2020-01-01T06:01:00Z", None).await;
    archive_input_and_update(&mut original, &store, owner.fence(), &contract).await;
    outbox.restore_diagnostics(checkpoint(&store).await).await.unwrap();
    let episode = outbox.diagnostic_checkpoints(1, None, 10).await.unwrap().checkpoints[0].episodes[0].clone();
    assert_eq!(episode.id, prior.id);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Pending);
    assert_eq!(episode.last_seen_at, prior.last_seen_at);
}
#[tokio::test]
async fn interrupted_checkpoint_installation_keeps_the_previous_working_set_and_atomic_retry() {
    let root = tempfile::tempdir().unwrap();
    let (store, owner, contract, mut original) = super::eviction::remote_box(root.path()).await;
    super::eviction::activity(&mut original, "2020-01-01T06:00:00Z", Some(observation())).await;
    archive_input_and_update(&mut original, &store, owner.fence(), &contract).await;
    let mut outbox =
        Outbox::create_remote(&root.path().join("replacement"), OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    outbox.restore_diagnostics(checkpoint(&store).await).await.unwrap();
    let before = outbox.diagnostic_checkpoints(1, None, 10).await.unwrap().checkpoints;
    super::eviction::activity(&mut original, "2020-01-01T06:01:00Z", None).await;
    archive_input_and_update(&mut original, &store, owner.fence(), &contract).await;
    journal_fault(
        &outbox,
        "CREATE TRIGGER fail_head BEFORE UPDATE ON diagnostic_heads BEGIN SELECT RAISE(ABORT,'test-stop'); END",
    )
    .await;
    assert!(outbox.restore_diagnostics(checkpoint(&store).await).await.is_err());
    assert_eq!(outbox.diagnostic_checkpoints(1, None, 10).await.unwrap().checkpoints, before);
    journal_fault(&outbox, "DROP TRIGGER fail_head").await;
    outbox.restore_diagnostics(checkpoint(&store).await).await.unwrap();
    assert_ne!(outbox.diagnostic_checkpoints(1, None, 10).await.unwrap().checkpoints, before);
}
#[tokio::test]
async fn exact_checkpoint_identity_complete_object_eof_and_restored_input_checksum_are_required() {
    let root = tempfile::tempdir().unwrap();
    let (store, owner, contract, mut original) = super::eviction::remote_box(root.path()).await;
    let native = super::eviction::activity(&mut original, "2020-01-01T06:00:00Z", Some(observation())).await;
    let committed = archive_input_and_update(&mut original, &store, owner.fence(), &contract).await;
    let head = store.diagnostic_heads(None, 1).await.unwrap().heads.remove(0);
    let mut wrong = head.clone();
    wrong.manifest_digest = digest(b"wrong");
    assert!(load_diagnostic_checkpoint(&store, &wrong).await.is_err());
    let batch = committed
        .manifest()
        .batches
        .iter()
        .find(|batch| batch.stream == TelemetryStream::DiagnosticCheckpoints)
        .unwrap();
    let path = root.path().join("recovery").join(&batch.object);
    let body = std::fs::read(&path).unwrap();
    let mut changed = body.clone();
    let last = changed.len() - 1;
    changed[last] = b' ';
    std::fs::write(&path, &changed).unwrap();
    assert!(load_diagnostic_checkpoint(&store, &head).await.is_err());
    std::fs::write(&path, body).unwrap();
    let original = original.delivery().prepare(&native.capture.observation_id, contract, vec![]).await.unwrap();
    let recovered = load_diagnostic_input(&store, &original.manifest().range_id).await.unwrap();
    let mut outbox =
        Outbox::create_remote(&root.path().join("replacement"), OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    let reference = checkpoint(&store).await.checkpoint().episodes[0].first.clone();
    outbox.cache_diagnostic_evidence("renamed", recovered).await.unwrap();
    journal_fault(&outbox, "UPDATE diagnostic_inputs SET restored_json='[]'").await;
    assert!(
        outbox.diagnostic_evidence(&reference).await.is_err(),
        "valid JSON corruption is not an empty successful baseline"
    );
}
#[tokio::test]
async fn volume_loss_preserves_uncertain_signal_without_freshening_last_positive_evidence() {
    let root = tempfile::tempdir().unwrap();
    let (store, owner, contract, mut outbox) = super::eviction::remote_box(root.path()).await;
    let mut row = observation();
    row.elapsed_ms = Some("45000".into());
    row.wait = Some(ActivityWait {
        identified_resources: vec![],
        native_type: Some("native-lock".into()),
        elapsed_ms: Some("40000".into()),
        resources: None,
        owners: vec![ActivityWaitOwner::Transaction {
            native_id: "transaction-2".into(),
            owner: TransactionWaitOwner::Active,
        }],
    });
    super::eviction::activity(&mut outbox, "2020-01-01T06:00:00Z", Some(row.clone())).await;
    archive_input_and_update(&mut outbox, &store, owner.fence(), &contract).await;
    let previous = checkpoint(&store).await.checkpoint().episodes.clone();
    assert_eq!(previous.len(), 1);
    row.blocking_status = ActivityBlockingStatus::Unknown;
    super::eviction::activity(&mut outbox, "2020-01-01T06:00:01Z", Some(row)).await;
    archive_input_and_update(&mut outbox, &store, owner.fence(), &contract).await;
    let recovered = checkpoint(&store).await;
    let next = &recovered.checkpoint().episodes[0];
    assert_eq!(next.lifecycle, DiagnosticLifecycle::Uncertain);
    assert_eq!(next.id, previous[0].id);
    assert_eq!(next.last_seen_at, previous[0].last_seen_at);
    assert_eq!(next.latest, previous[0].latest);
    let mut restored =
        Outbox::create_remote(&root.path().join("replacement"), OutboxLimits::default(), store.id()).unwrap();
    restored.recover().await.unwrap();
    restored.restore_diagnostics(recovered).await.unwrap();
    assert_eq!(
        restored.diagnostic_checkpoints(1, None, 10).await.unwrap().checkpoints[0].episodes[0].lifecycle,
        DiagnosticLifecycle::Uncertain
    );
}
