use super::*;
use std::sync::atomic::Ordering;

mod non_index;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn native_maintenance_progress_preserves_original_actor_scope_role_phase_and_unknown_completion() {
    for (source, records) in test_support::maintenance_progress_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let capture =
            fixture.reader().read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        let SourceObservation::MaintenanceProgress { result, snapshot } = &capture.source().observation else {
            panic!("native maintenance source");
        };
        assert_eq!(snapshot.dataset, SnapshotDataset::MaintenanceProgress);
        assert!(result.focus.is_some());
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
        for row in capture.records() {
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::MaintenanceProgress(point),
                ..
            }) = &row.row().record
            else {
                panic!("native maintenance point")
            };
            assert_eq!(&point.actor, result.expected_actor().unwrap());
            if point.command == MaintenanceCommand::Cluster {
                assert_eq!(point.index_reference().unwrap().id, "16422");
            } else {
                assert!(point.index_reference().is_none(), "native zero or missing index remains unresolved");
            }
            assert!(!serde_json::to_string(point).unwrap().contains("completed_at"));
        }
    }
}

#[tokio::test]
async fn an_archive_gap_keeps_the_original_positive_progress_and_native_membership_immutable() {
    let (source, records) = test_support::maintenance_progress_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.replies(vec![vec![]]);
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let capture =
        fixture.reader().read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.gaps().len(), 1);
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
    let SourceObservation::MaintenanceProgress { result, snapshot } = &capture.source().observation else {
        panic!("native maintenance source");
    };
    assert_eq!(snapshot.records, 1);
    assert_eq!(result.membership, FocusedRequestMembership::Observed);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn changed_actor_object_or_work_is_rejected_before_recovery_can_replace_original_evidence() {
    let (source, records) = test_support::maintenance_progress_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    for (pointer, value) in [
        ("/actor/request/generation", json!("recycled-request")),
        ("/statement/id", json!("another-statement")),
        ("/relation_oid", json!("16500")),
        ("/work/progress/blocks_total", json!("9007199254740992")),
        ("/observed_at", json!("2020-01-01T11:00:00Z")),
    ] {
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).expect("native point field") = value;
        rows[0][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(
                &fixture.range,
                FAMILY,
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
    }
}
