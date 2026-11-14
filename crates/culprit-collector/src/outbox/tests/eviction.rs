use super::*;
use crate::delivery::{
    contract::Contract,
    file_store::{FileOwnership, FileRecoveryStore},
    recovery::*,
};

pub(super) async fn remote_box(root: &Path) -> (FileRecoveryStore, FileOwnership, Contract, Outbox) {
    let contract = Contract::new("pipeline".into(), "shard-0".into(), "file:test".into(), digest(b"schema")).unwrap();
    let store = FileRecoveryStore::create(&root.join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut outbox = Outbox::create_remote(&root.join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    outbox.recover().await.unwrap();
    outbox.begin_remote_epoch(owner.fence(), 0).await.unwrap();
    (store, owner, contract, outbox)
}

#[tokio::test]
async fn failure_after_unlink_resumes_from_durable_intent_without_clearing_source_debt() {
    let root = tempfile::TempDir::new().unwrap();
    let (store, owner, contract, mut outbox) = remote_box(root.path()).await;
    let manifest = complete(&mut outbox).await;
    let delivery = outbox.delivery();
    let prepared = delivery.prepare(&manifest.capture.observation_id, contract, vec![]).await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    journal_fault(
        &outbox,
        "CREATE TRIGGER fail_evict BEFORE DELETE ON captures BEGIN SELECT RAISE(FAIL,'test-stop'); END",
    )
    .await;
    assert_eq!(kind(&delivery.evict(&committed).await.err().unwrap()), OutboxProblem::Storage);
    let status = outbox.status(50, None).await.unwrap();
    assert_eq!(status.entries[0].state, OutboxCaptureState::Evicting);
    assert_eq!(status.entries[0].source_work, SourceWorkState::Quiescent);
    assert!(!root.path().join("outbox/captures").join(&manifest.capture.observation_id).exists());
    journal_fault(&outbox, "DROP TRIGGER fail_evict").await;
    drop(delivery);
    drop(outbox);
    let mut outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    outbox.recover().await.unwrap();
    assert_eq!(outbox.status(50, None).await.unwrap().captures, 0);
    assert_eq!(store.load(&committed.manifest().range_id).await.unwrap(), *committed.manifest());
}

#[tokio::test]
async fn wrong_recovery_store_and_unconfirmed_work_never_authorize_deletion() {
    let root = tempfile::TempDir::new().unwrap();
    let (store, owner, contract, mut outbox) = remote_box(root.path()).await;
    let manifest = complete(&mut outbox).await;
    let delivery = outbox.delivery();
    let prepared = delivery.prepare(&manifest.capture.observation_id, contract.clone(), vec![]).await.unwrap();
    let other = FileRecoveryStore::create(&root.path().join("other"), contract).unwrap();
    let other_owner = other.acquire().await.unwrap();
    let foreign = stage_and_commit(&other, other_owner.fence(), None, &prepared).await.unwrap();
    assert_eq!(
        delivery.evict(&foreign).await.err().unwrap().detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::DestinationChanged))
    );
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    journal_fault(&outbox, r#"UPDATE captures SET source_work='"unconfirmed"'"#).await;
    assert_eq!(kind(&delivery.evict(&committed).await.err().unwrap()), OutboxProblem::SourceUnconfirmed);
    assert!(root.path().join("outbox/captures").join(&manifest.capture.observation_id).exists());
    assert_eq!(outbox.status(50, None).await.unwrap().source_debts, 1);
}

async fn empty_activity(outbox: &mut Outbox, at: &str) -> CollectionManifest {
    activity(outbox, at, None).await
}
pub(super) async fn activity(
    outbox: &mut Outbox,
    at: &str,
    observation: Option<ActivityObservation>,
) -> CollectionManifest {
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Activity,
        budget: Default::default(),
    };
    let mut lease = outbox.reserve("fixture", &request).await.unwrap();
    let mut source = source();
    source.source = "live_requests".into();
    source.observation = SourceObservation::Activity(ActivitySnapshotDescription {
        population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
        epoch: Some("native-activity-epoch".into()),
        observed_from: at.into(),
        observed_until: at.into(),
        at: at.into(),
        scope: culprit_core::MeasurementScope::Database,
        native_rows: u32::from(observation.is_some()),
        observations: u32::from(observation.is_some()),
        coverage: StateSourceCoverage::collected("live_requests".into(), u32::from(observation.is_some())),
    });
    let mut files = ObservationFiles::create(lease.directory(), request, lease.identity().clone(), None).unwrap();
    lease.source_started().await.unwrap();
    files.describe(source).unwrap();
    if let Some(observation) = observation {
        files.begin_page(0).unwrap();
        assert!(files.write_page(0, &[TelemetryRecord::from(observation)]).unwrap().is_none());
    }
    files.seal(0).unwrap();
    files.confirm_source(SourceCompletion::Verified).unwrap();
    lease.source_closed(SourceShutdown::Quiescent).await.unwrap();
    let manifest = files.finish(0).unwrap();
    lease.finish().await.unwrap();
    manifest
}

#[tokio::test]
async fn a_native_baseline_is_pinned_until_its_diagnostic_input_has_a_durable_successor() {
    let root = tempfile::TempDir::new().unwrap();
    let (store, owner, contract, mut outbox) = remote_box(root.path()).await;
    let first = empty_activity(&mut outbox, "2020-01-01T06:00:00Z").await;
    let delivery = outbox.delivery();
    let prepared = delivery.prepare(&first.capture.observation_id, contract.clone(), vec![]).await.unwrap();
    let native = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    assert!(!delivery.evict(&native).await.unwrap());
    let input = outbox.next_diagnostic_input(1).await.unwrap().unwrap();
    let derived = outbox
        .commit_diagnostics(input.clone(), super::restored_diagnostics::pending_update(&input, "initial"))
        .await
        .unwrap();
    let prepared = delivery.prepare(&derived.observation_id, contract, vec![]).await.unwrap();
    let checkpoint =
        stage_and_commit(&store, owner.fence(), Some(&native.manifest().range_id), &prepared).await.unwrap();
    assert!(!delivery.evict(&checkpoint).await.unwrap());
    delivery.mark_archived(&checkpoint).await.unwrap();
    assert!(delivery.evict(&native).await.unwrap());
    assert_eq!(outbox.status(50, None).await.unwrap().captures, 0);
}

#[tokio::test]
async fn archived_baseline_markers_survive_restart_and_do_not_rearchive_the_protected_head() {
    let root = tempfile::tempdir().unwrap();
    let (store, owner, contract, mut outbox) = remote_box(root.path()).await;
    let first = empty_activity(&mut outbox, "2020-01-01T06:00:00Z").await;
    let id = &first.capture.observation_id;
    let delivery = outbox.delivery();
    let prepared = delivery.prepare(id, contract, vec![]).await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    assert_eq!(delivery.next_archive().await.unwrap().as_ref(), Some(id));
    delivery.mark_archived(&committed).await.unwrap();
    assert!(delivery.next_archive().await.unwrap().is_none());
    assert_eq!(outbox.status(50, None).await.unwrap().captures, 1, "latest complete baseline stays local");
    drop(delivery);
    drop(outbox);
    let mut outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    outbox.recover().await.unwrap();
    let delivery = outbox.delivery();
    assert!(delivery.next_archive().await.unwrap().is_none());
    assert_eq!(delivery.archive_range(id).await.unwrap().as_deref(), Some(committed.manifest().range_id.as_str()));
    delivery.mark_archived(&committed).await.unwrap();
    drop(owner);
    let owner = store.acquire().await.unwrap();
    outbox.begin_remote_epoch(owner.fence(), 1).await.unwrap();
    let input = outbox.next_diagnostic_input(1).await.unwrap().unwrap();
    let derived = outbox
        .commit_diagnostics(input.clone(), super::restored_diagnostics::pending_update(&input, "initial"))
        .await
        .unwrap();
    let prepared =
        delivery.prepare(&derived.observation_id, committed.manifest().contract.clone(), vec![]).await.unwrap();
    let checkpoint =
        stage_and_commit(&store, owner.fence(), Some(&committed.manifest().range_id), &prepared).await.unwrap();
    delivery.mark_archived(&checkpoint).await.unwrap();
    assert!(delivery.evict(&committed).await.unwrap());
    assert_eq!(outbox.status(50, None).await.unwrap().captures, 0);
}

#[tokio::test]
async fn an_archive_handoff_interrupted_after_the_marker_keeps_its_commit_and_finishes_eviction_on_recovery() {
    let root = tempfile::tempdir().unwrap();
    let (store, owner, contract, mut outbox) = remote_box(root.path()).await;
    let first = complete(&mut outbox).await;
    let delivery = outbox.delivery();
    let prepared = delivery.prepare(&first.capture.observation_id, contract, vec![]).await.unwrap();
    let committed = stage_and_commit(&store, owner.fence(), None, &prepared).await.unwrap();
    journal_fault(
        &outbox,
        "CREATE TRIGGER fail_archive_evict BEFORE DELETE ON captures BEGIN SELECT RAISE(FAIL,'test-stop'); END",
    )
    .await;
    assert!(delivery.mark_archived(&committed).await.is_err());
    assert_eq!(
        delivery.archive_range(&first.capture.observation_id).await.unwrap().as_deref(),
        Some(committed.manifest().range_id.as_str())
    );
    assert_eq!(outbox.status(50, None).await.unwrap().entries[0].state, OutboxCaptureState::Evicting);
    journal_fault(&outbox, "DROP TRIGGER fail_archive_evict").await;
    drop(delivery);
    drop(outbox);
    let mut outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    outbox.recover().await.unwrap();
    assert_eq!(outbox.status(50, None).await.unwrap().captures, 0);
    let committed = reopen(&store, &committed.manifest().range_id).await.unwrap();
    outbox.delivery().mark_archived(&committed).await.unwrap();
    assert_eq!(store.recover(None, 50).await.unwrap().ranges, [prepared.manifest().clone()]);
}
