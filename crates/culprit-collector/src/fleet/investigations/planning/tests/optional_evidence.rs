use super::*;
use crate::delivery::{contract::Contract, file_store::FileRecoveryStore};
use culprit_core::telemetry::{OutboxProblem, batch_id, digest};

fn absent(reference: &DiagnosticEvidenceRef) -> DiagnosticEvidenceRef {
    let mut reference = reference.clone();
    reference.capture.observation_id = "retired-native-cost-capture".into();
    reference.batch_id = batch_id(&reference.capture.observation_id, reference.stream).unwrap();
    assert!(reference.is_valid());
    reference
}

#[tokio::test]
async fn a_missing_optional_cost_capture_withholds_planning_without_source_work_or_new_history() {
    let mut harness = Harness::new(1).await;
    let RecipeCostQualification::Measured(receipt) = &mut harness.backend.recipe.cost else { panic!() };
    receipt.evidence = absent(&receipt.evidence);
    let before = harness.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    assert!(harness.prepare(&CollectionCancellation::default()).await.unwrap().is_empty());
    let after = harness.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    assert_eq!(after.checkpoints, before.checkpoints);
    assert!(harness.journal.focused_intents().await.unwrap().is_empty());
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.calls.opened.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.calls.captured.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_uncommitted_archive_capture_is_temporarily_unavailable_without_becoming_cancellation_or_native_work() {
    let archive = tempfile::tempdir().unwrap();
    let contract =
        Contract::new("pipeline".into(), "shard".into(), "file:warehouse".into(), digest(b"optional-evidence-schema"))
            .unwrap();
    let recovery = FileRecoveryStore::create(&archive.path().join("recovery"), contract).unwrap();
    let owner = recovery.acquire().await.unwrap();
    let mut harness = Harness::with_recovery_store(1, Some(owner.fence())).await;
    let RecipeCostQualification::Measured(receipt) = &harness.backend.recipe.cost else { panic!() };
    let reference = absent(&receipt.evidence);
    assert_eq!(
        recovery.capture_manifest(&reference.capture).await.unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::NotReady))
    );
    let cancellation = CollectionCancellation::default();
    assert!(
        resolve_optional_evidence(&mut harness.outbox, "saved-source", &reference, Some(&recovery), &cancellation)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!cancellation.is_cancelled());
    assert!(harness.journal.focused_intents().await.unwrap().is_empty());
    assert_eq!(harness.backend.calls.opened.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.calls.captured.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_forged_optional_receipt_still_fails_integrity_before_cost_verification_or_source_work() {
    let mut harness = Harness::new(1).await;
    let RecipeCostQualification::Measured(receipt) = &mut harness.backend.recipe.cost else { panic!() };
    receipt.evidence.payload_sha256 = digest(b"forged native experiment payload");
    let error = harness.prepare(&CollectionCancellation::default()).await.err().unwrap();
    assert_eq!(error.detail, Some(ErrorDetail::Outbox(OutboxProblem::Integrity)));
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.calls.opened.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.calls.captured.load(Ordering::SeqCst), 0);
    assert!(harness.journal.focused_intents().await.unwrap().is_empty());
}
