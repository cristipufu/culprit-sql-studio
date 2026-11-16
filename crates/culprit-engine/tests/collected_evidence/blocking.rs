use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{BlockingCase, assert_blocking_capture, blocking_evidence, blocking_evidence_cases},
};
use culprit_core::{HeadKind, MaintenanceDirection, SourceState, telemetry::*};
use culprit_engine::collected_resources;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ActivityObservations];

#[tokio::test]
async fn exact_original_focus_and_native_owner_closures_use_the_same_offline_explanation_without_native_or_payload_reads()
 {
    for (source, records) in blocking_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_blocking_capture(&capture, &source, &records);
        let actual = collected_resources::blocking(&capture).unwrap();
        let references = actual
            .context
            .records
            .iter()
            .map(|row| {
                DiagnosticEvidenceRef::from_export(
                    &source,
                    capture.capture_manifest_sha256(),
                    &row.row,
                    row.row_ordinal - 1,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let rows = actual
            .context
            .records
            .iter()
            .zip(&references)
            .map(|(row, reference)| {
                let TelemetryRecord::Activity(observation) = &row.row.record else {
                    panic!("typed native participant")
                };
                ActivityFrameRow { observation, reference: Some(reference) }
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::focused_blocking::explain(&source, &rows, true).unwrap();
        assert_eq!(serde_json::to_value(&actual.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(actual.context.source, source);
        assert_eq!(actual.report.native_rows as usize, records.len());
        assert_eq!(actual.report.observed_rows as usize, records.len());
        assert!(actual.report.read_complete);
        assert_eq!(actual.context.metrics.recovery_objects, 0);
        assert_eq!(fixture.recovery_object_reads(), opened);
        let value = serde_json::to_value(&actual).unwrap();
        assert!(value.get("snapshot").is_none() && value.get("workload").is_none());
    }
}

#[tokio::test]
async fn idle_prepared_and_maintenance_owners_remain_distinct_with_no_statement_as_lock_acquirer_or_action_target() {
    let (source, records) = blocking_evidence(BlockingCase::IdleOwner);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(FAMILY).await;
    let report = collected_resources::blocking(&capture).unwrap().report;
    let head = &report.blocking.unwrap().chains[0];
    assert_eq!(head.head_kind, HeadKind::IdleInTransaction);
    assert_eq!(head.tree.session, "43");
    let context = head.tree.context.as_ref().unwrap();
    assert!(context.sql.is_none() && context.action_target.is_none());

    let (source, records) = blocking_evidence(BlockingCase::PreparedOwner);
    let fixture = Fixture::new(source, records).await;
    let report = collected_resources::blocking(&fixture.read_families(FAMILY).await).unwrap().report;
    let head = &report.blocking.unwrap().chains[0];
    assert!(
        matches!(&head.tree.owner, Some(ActivityWaitOwner::Transaction { native_id, owner: TransactionWaitOwner::Prepared }) if native_id == "0")
    );
    assert!(head.tree.context.is_none());
    assert!(!report.relationships[0].owner_observed);

    let (source, records) = blocking_evidence(BlockingCase::Matched);
    let fixture = Fixture::new(source, records).await;
    let report = collected_resources::blocking(&fixture.read_families(FAMILY).await).unwrap().report;
    let maintenance = report.maintenance.unwrap();
    assert_eq!(maintenance.entries[0].direction, MaintenanceDirection::BlockingSession);
    assert_eq!(maintenance.entries[0].affected_count, 1);
    assert_eq!(maintenance.entries[0].reference.evidence.as_ref().unwrap().row_ordinal, 1);
}

#[tokio::test]
async fn missing_waiter_or_entire_warehouse_body_is_a_gap_not_healthy_absence_or_recovered_incident() {
    let (source, records) = blocking_evidence(BlockingCase::Matched);
    let fixture = Fixture::new(source.clone(), records).await;
    let opened = fixture.recovery_object_reads();
    fixture.remove_first_row(TelemetryStream::ActivityObservations);
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::blocking(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_rows, 2);
    assert_eq!(analysis.report.observed_rows, 1);
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert!(!analysis.report.read_complete);
    assert!(analysis.report.relationships.is_empty() && analysis.report.blocking.is_none());
    assert_eq!(analysis.report.maintenance.as_ref().unwrap().entries[0].direction, MaintenanceDirection::Unlinked);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    fixture.remove_family(TelemetryStream::ActivityObservations);
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::blocking(&capture).unwrap();
    assert_eq!(analysis.report.native_rows, 2);
    assert_eq!(analysis.report.observed_rows, 0);
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert!(!analysis.report.read_complete);
    assert!(analysis.report.blocking.is_none() && analysis.report.maintenance.is_none());
    assert_eq!(fixture.recovery_object_reads(), opened, "readback does not resurrect missing native participants");
}

#[tokio::test]
async fn current_owner_authority_does_not_upgrade_target_originals_or_hide_final_request_changes() {
    for case in [BlockingCase::UnverifiedOriginal, BlockingCase::RequestChanged, BlockingCase::RequestMissing] {
        let (source, records) = blocking_evidence(case);
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read_families(FAMILY).await;
        let report = collected_resources::blocking(&capture).unwrap().report;
        assert_eq!(report.relationships.len(), 1);
        assert!(report.blocking.is_some(), "the current native frame explains its own relationships");
        if case == BlockingCase::UnverifiedOriginal {
            assert_eq!(report.binding, FocusedRequestBinding::UnverifiedOriginalScope);
        } else {
            assert!(matches!(report.request_end, BlockingRequestState::Changed | BlockingRequestState::Missing));
        }
        assert_eq!(report.source_state, SourceState::Partial);
    }
}

#[tokio::test]
async fn an_unselected_activity_family_and_an_ordinary_unfocused_frame_cannot_become_a_blocking_closure() {
    let (source, records) = blocking_evidence(BlockingCase::Matched);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(collected_resources::blocking(&capture).is_err());
    let (source, records) = culprit_collector::analytical::test_support::activity_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    assert!(collected_resources::blocking(&fixture.read_families(FAMILY).await).is_err());
}
