//! Actual durable Ready/control boundaries survive a lost process without opening a replacement source.
use super::*;
use crate::{
    delivery::{
        contract::Contract,
        diagnostic_recovery::{load_diagnostic_checkpoint, load_diagnostic_input},
        file_store::FileRecoveryStore,
        recovery::{CommittedRange, Fence, RecoveryStore, stage_and_commit},
    },
    fleet::{
        FleetJournal,
        investigations::Investigations,
        scheduler::{FocusStatus, Scheduler},
    },
    outbox::Outbox,
};
use tokio::time::Instant;

mod qualification_pins;

async fn restart(runtime: Runtime, expected_source_debts: u32) -> Runtime {
    let Harness { _directory, inventory, outbox, journal, bindings, backend } = runtime.fixture;
    drop(outbox);
    drop(journal);
    let mut outbox = Outbox::open(&_directory.path().join("outbox")).unwrap();
    let recovered = outbox.recover().await.unwrap();
    assert_eq!((recovered.interrupted, recovered.quarantined, recovered.source_debts), (0, 0, expected_source_debts));
    let journal = FleetJournal::open(&_directory.path().join("fleet"), &inventory, "one").await.unwrap();
    let started = Instant::now();
    let mut scheduler = Scheduler::new(&inventory, "one").unwrap();
    scheduler.restore(journal.scheduler().await.unwrap().unwrap(), started.elapsed(), chrono::Utc::now()).unwrap();
    if let Some(update) = scheduler.resumed_admission_checkpoint(started.elapsed(), chrono::Utc::now()).unwrap() {
        scheduler.persisted(journal.update_scheduler(update).await.unwrap()).unwrap();
    }
    let investigations = Investigations::new(&inventory, "one");
    Runtime {
        fixture: Harness { _directory, inventory, outbox, journal, bindings, backend },
        investigations,
        scheduler,
        started,
    }
}

#[tokio::test]
async fn ready_before_completion_survives_restart_and_settles_only_after_exact_offline_consumption() {
    for quiet in [false, true] {
        let mut runtime = Runtime::from_fixture(fixture().await).await;
        let original = gateway_answer(&mut runtime).await;
        if quiet {
            runtime.complete_empty_baseline().await;
        }
        assert!(runtime.enroll().await);
        let job = runtime.take_focus().await;
        let result = runtime.execute(&job).await;
        let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
        let before = runtime.native_calls();
        // The actual native owner has closed and Ready enrolled the control, but the Fleet completion
        // acknowledgement has not been written. A restored scheduler must initially retain uncertainty.
        let mut runtime = restart(runtime, 0).await;
        let state = runtime.scheduler.focused_state(&job.key).unwrap();
        assert_eq!(state.status, FocusStatus::Pending);
        assert_eq!(state.source_work, SourceWorkState::Unconfirmed);
        assert_eq!(
            runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
            DiagnosticInputState::Pending
        );
        runtime.reconcile().await;
        assert_eq!(runtime.scheduler.focused_state(&job.key).unwrap().source_work, SourceWorkState::Unconfirmed);
        let policies = runtime.policies();
        assert_eq!(
            analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
                .await
                .unwrap(),
            1
        );
        let revisions = runtime.revisions(&capture.capture).await;
        let [revision] = revisions.as_slice() else { panic!() };
        assert_eq!(revision.episode.report, original.report);
        assert_eq!(revision.episode.analysis, original.analysis);
        assert_eq!(revision.episode.source_read_capture(), Some(&capture));
        runtime.reconcile().await;
        assert!(matches!(runtime.scheduler.focused_state(&job.key).unwrap().status, FocusStatus::Completed { .. }));
        runtime.reconcile().await;
        assert!(runtime.scheduler.focused_state(&job.key).is_none());
        assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
        assert_eq!(
            analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
                .await
                .unwrap(),
            0
        );
        assert_eq!(runtime.native_calls(), before);
    }
}

#[tokio::test]
async fn enrollment_before_dispatch_survives_restart_without_current_route_authority_or_native_work() {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    let original = gateway_answer(&mut runtime).await;
    assert!(runtime.enroll().await);
    let before = runtime.native_calls();
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    let [intent] = intents.as_slice() else { panic!() };
    let intent = intent.clone();
    let mut runtime = restart(runtime, 0).await;
    runtime.reconcile().await;
    assert!(!runtime.enroll().await, "a saved route is not a current source execution owner");
    assert_eq!(runtime.fixture.journal.focused_intent(&intent.key().unwrap()).await.unwrap(), Some(intent.clone()));
    assert_eq!(episode(&mut runtime).await.report, original.report);
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().contains(&intent.key().unwrap()));
    assert_eq!(runtime.native_calls(), before);
}

#[tokio::test]
async fn cold_pending_control_replays_exact_original_and_gateway_proofs_with_no_source_credentials() {
    let archive = tempfile::tempdir().unwrap();
    let contract =
        Contract::new("pipeline".into(), "one".into(), "file:source-read".into(), digest(b"source-read-schema"))
            .unwrap();
    let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut runtime = Runtime::from_fixture(fixture_with_fence(Some(owner.fence())).await).await;
    let original = gateway_answer(&mut runtime).await;
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
    let before = runtime.native_calls();
    let archived = archive_ready(&mut runtime, &store, owner.fence(), &contract).await;
    let result_range = &archived[&capture.capture.observation_id].manifest().range_id;
    let heads = store.diagnostic_heads(None, 10).await.unwrap();
    assert_eq!(heads.heads.len(), 1);
    let directory = tempfile::tempdir().unwrap();
    let mut cold =
        Outbox::create_remote(&directory.path().join("outbox"), OutboxLimits::default(), store.id()).unwrap();
    cold.recover().await.unwrap();
    cold.restore_diagnostics(load_diagnostic_checkpoint(&store, &heads.heads[0]).await.unwrap()).await.unwrap();
    let recovered = load_diagnostic_input(&store, result_range).await.unwrap();
    cold.restore_diagnostic_input("saved-source", recovered, &store).await.unwrap();
    let high_sequence = store.recover(None, 1).await.unwrap().high_sequence;
    cold.begin_diagnostic_epoch(owner.fence(), high_sequence).await.unwrap();
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut cold, &policies, &CollectionCancellation::default(), 10, Some(&store)).await.unwrap(),
        1
    );
    let input = cold.diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    assert!(input.rows.is_empty());
    let answer = input.checkpoint.as_ref().unwrap().episodes.iter().find(|episode| episode.id == original.id).unwrap();
    assert_eq!(answer.first, original.first);
    assert_eq!(answer.report, original.report);
    assert_eq!(answer.analysis, original.analysis);
    assert_eq!(answer.source_read_capture(), Some(&capture));
    assert_eq!(runtime.native_calls(), before);
}

async fn archive_ready(
    runtime: &mut Runtime,
    store: &FileRecoveryStore,
    fence: &Fence,
    contract: &Contract,
) -> std::collections::BTreeMap<String, CommittedRange> {
    let mut entries = runtime.fixture.outbox.status(100, None).await.unwrap().entries;
    entries.sort_by_key(|entry| entry.capture.revision.as_ref().unwrap().sequence.parse::<u64>().unwrap());
    let mut archived = std::collections::BTreeMap::new();
    for entry in entries.into_iter().filter(|entry| entry.state == OutboxCaptureState::Ready) {
        let prepared = runtime
            .fixture
            .outbox
            .delivery()
            .prepare(&entry.capture.observation_id, contract.clone(), vec![])
            .await
            .unwrap();
        let head = store.checkpoint_head().await.unwrap();
        let committed = stage_and_commit(store, fence, head.as_deref(), &prepared).await.unwrap();
        runtime.fixture.outbox.delivery().mark_archived(&committed).await.unwrap();
        archived.insert(entry.capture.observation_id, committed);
    }
    archived
}

#[tokio::test]
async fn archived_quiet_result_releases_its_one_capture_pin_and_charge_after_history_and_queue_acknowledgements() {
    let archive = tempfile::tempdir().unwrap();
    let contract = Contract::new(
        "pipeline".into(),
        "one".into(),
        "file:source-read-retirement".into(),
        digest(b"source-read-retirement-schema"),
    )
    .unwrap();
    let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let mut runtime = Runtime::from_fixture(fixture_with_fence(Some(owner.fence())).await).await;
    gateway_answer(&mut runtime).await;
    runtime.complete_empty_baseline().await;
    assert!(runtime.enroll().await);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
    runtime.finish(Outcome::FocusedComplete { result: capture.clone() }).await;
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    runtime.reconcile().await;
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
    // A later genuine quiet baseline advances the working-set head; immutable history retains the answer.
    runtime.complete_empty_baseline().await;
    let archived = archive_ready(&mut runtime, &store, owner.fence(), &contract).await;
    let result = &archived[&capture.capture.observation_id];
    assert!(runtime.fixture.outbox.delivery().evict(result).await.unwrap());
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Missing
    );
    let charged = runtime.fixture.outbox.status(100, None).await.unwrap().charged_bytes;
    // Eviction reports settled absence, including an already absent capture; it is not a deletion counter.
    assert!(runtime.fixture.outbox.delivery().evict(result).await.unwrap());
    assert!(
        !runtime
            .fixture
            .outbox
            .status(100, None)
            .await
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.capture == capture.capture)
    );
    assert_eq!(runtime.fixture.outbox.status(100, None).await.unwrap().charged_bytes, charged);
    let retained = load_diagnostic_input(&store, &result.manifest().range_id).await.unwrap();
    assert_eq!(retained.range().manifest().capture, capture.capture);
    assert!(retained.range().manifest().source.observation.execution_context().is_some());
}
