//! Cost rows are sealed originals; the controlled counters grant no production qualification.
use super::*;

async fn enrolled_question(fence: &Fence, history: bool) -> (Runtime, DiagnosticInvestigationIntent) {
    let mut runtime = Runtime::from_fixture(fixture_with_fence(Some(fence)).await).await;
    if history {
        gateway_answer(&mut runtime).await;
    }
    assert!(runtime.enroll().await);
    let mut intents = runtime.fixture.journal.focused_intents().await.unwrap();
    assert_eq!(intents.len(), 1);
    let intent = intents.remove(0);
    assert_eq!(matches!(intent.focus, DiagnosticFocus::SourceRead { .. }), history);
    let reference = &intent.qualification.evidence;
    assert_ne!(reference.capture, intent.focus.parent().capture);
    assert_eq!(reference.row_ordinal, 0);
    let (source, row) = runtime.fixture.outbox.diagnostic_evidence(reference).await.unwrap();
    reference.validate_canonical_record(&source, &row.record).unwrap();
    assert_eq!(
        DiagnosticEvidenceRef::from_export(&source, &reference.manifest_sha256, &row, reference.row_ordinal).unwrap(),
        *reference
    );
    let mut wrong_ordinal = reference.clone();
    wrong_ordinal.row_ordinal = 1;
    assert!(runtime.fixture.outbox.diagnostic_evidence(&wrong_ordinal).await.is_err());
    (runtime, intent)
}

async fn archived_cost(
    runtime: &mut Runtime,
    intent: &DiagnosticInvestigationIntent,
    store: &FileRecoveryStore,
    fence: &Fence,
    contract: &Contract,
) -> CommittedRange {
    let archived = archive_ready(runtime, store, fence, contract).await;
    let cost = archived[&intent.qualification.evidence.capture.observation_id].clone();
    assert_eq!(cost.manifest().capture, intent.qualification.evidence.capture);
    assert!(!runtime.fixture.outbox.delivery().evict(&cost).await.unwrap());
    cost
}

#[tokio::test]
async fn focused_and_history_cost_evidence_survives_restart_until_the_exact_queued_owner_retires() {
    for history in [false, true] {
        let archive = tempfile::tempdir().unwrap();
        let contract = Contract::new(
            "pipeline".into(),
            "one".into(),
            "file:qualification-retention".into(),
            digest(b"qualification-retention-schema"),
        )
        .unwrap();
        let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
        let owner = store.acquire().await.unwrap();
        let (mut runtime, intent) = enrolled_question(owner.fence(), history).await;
        let before = runtime.native_calls();
        runtime.fixture.outbox.pin_investigation_parent(&intent).await.unwrap();
        let cost = archived_cost(&mut runtime, &intent, &store, owner.fence(), &contract).await;
        let key = intent.key().unwrap();
        let mut runtime = restart(runtime, 0).await;
        assert_eq!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap(), vec![key.clone()]);
        assert!(!runtime.fixture.outbox.delivery().evict(&cost).await.unwrap());
        runtime.fixture.outbox.pin_investigation_parent(&intent).await.unwrap();
        runtime.fixture.outbox.release_investigation_parent(&key).await.unwrap();
        runtime.fixture.outbox.release_investigation_parent(&key).await.unwrap();
        assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
        assert!(runtime.fixture.outbox.delivery().evict(&cost).await.unwrap());
        assert!(runtime.fixture.outbox.diagnostic_evidence(&intent.qualification.evidence).await.is_err());
        assert_eq!(runtime.native_calls(), before, "retention never opens a native source");
    }
}

#[tokio::test]
async fn consumed_focused_and_history_results_release_cost_evidence_through_normal_fleet_retirement() {
    for history in [false, true] {
        let archive = tempfile::tempdir().unwrap();
        let contract = Contract::new(
            "pipeline".into(),
            "one".into(),
            "file:qualification-retirement".into(),
            digest(b"qualification-retirement-schema"),
        )
        .unwrap();
        let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
        let owner = store.acquire().await.unwrap();
        let (mut runtime, intent) = enrolled_question(owner.fence(), history).await;
        let cost = archived_cost(&mut runtime, &intent, &store, owner.fence(), &contract).await;
        let job = runtime.take_focus().await;
        let result = runtime.execute(&job).await;
        let Outcome::FocusedComplete { result: capture } = result.outcome else { panic!() };
        runtime.finish(Outcome::FocusedComplete { result: capture }).await;
        assert!(!runtime.fixture.outbox.delivery().evict(&cost).await.unwrap());
        let policies = runtime.policies();
        assert_eq!(
            analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
                .await
                .unwrap(),
            1
        );
        runtime.reconcile().await;
        assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
        let before = runtime.native_calls();
        assert!(runtime.fixture.outbox.delivery().evict(&cost).await.unwrap());
        runtime.reconcile().await;
        assert_eq!(runtime.native_calls(), before, "cost retirement does not dispatch another read");
    }
}

#[tokio::test]
async fn unconfirmed_history_keeps_its_cost_evidence_across_restart_without_retrying_native_work() {
    use crate::fleet::{admission::AdmittedBackend, storage::CaptureStorage};

    let archive = tempfile::tempdir().unwrap();
    let contract = Contract::new(
        "pipeline".into(),
        "one".into(),
        "file:qualification-unconfirmed".into(),
        digest(b"qualification-unconfirmed-schema"),
    )
    .unwrap();
    let store = FileRecoveryStore::create(&archive.path().join("recovery"), contract.clone()).unwrap();
    let owner = store.acquire().await.unwrap();
    let (mut runtime, intent) = enrolled_question(owner.fence(), true).await;
    let cost = archived_cost(&mut runtime, &intent, &store, owner.fence(), &contract).await;
    let job = runtime.take_focus().await;
    runtime.fixture.backend.focus_failure = Some(CollectionStop::CancellationUnconfirmed);
    {
        let admitted = AdmittedBackend::new(&runtime.fixture.backend, Instant::now());
        let result = runtime
            .investigations
            .execute(
                &runtime.fixture.backend,
                &admitted,
                &runtime.fixture.journal,
                &mut CaptureStorage::local(&mut runtime.fixture.outbox),
                &job,
                &CollectionCancellation::default(),
            )
            .await;
        assert_eq!(CollectionStop::from_error(&result.err().unwrap()), CollectionStop::CancellationUnconfirmed);
        assert_eq!(admitted.source_work(), SourceWorkState::Unconfirmed);
    }
    assert!(!runtime.fixture.outbox.delivery().evict(&cost).await.unwrap());
    let before = runtime.native_calls();
    let mut runtime = restart(runtime, 1).await;
    runtime.reconcile().await;
    assert_eq!(runtime.fixture.outbox.status(100, None).await.unwrap().unconfirmed_sources, 1);
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().contains(&intent.key().unwrap()));
    assert!(!runtime.fixture.outbox.delivery().evict(&cost).await.unwrap());
    assert_eq!(runtime.native_calls(), before);
}
