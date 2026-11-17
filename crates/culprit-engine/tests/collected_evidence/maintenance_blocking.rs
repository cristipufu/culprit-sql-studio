use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{MaintenanceBlockingCase, MaintenanceBlockingFixture, maintenance_blocking_evidence},
};
use culprit_core::{SourceState, telemetry::*};
use culprit_engine::collected_resources;

const PROGRESS: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
const BLOCKING: &[EvidenceFamily] = &[EvidenceFamily::ActivityObservations];

async fn fixtures(evidence: MaintenanceBlockingFixture) -> (Fixture, Fixture) {
    (
        Fixture::with_sequence(evidence.maintenance_source, evidence.maintenance_rows, 100_001).await,
        Fixture::with_sequence(evidence.blocking_source, evidence.blocking_rows, 100_002).await,
    )
}

#[tokio::test]
async fn two_authenticated_reads_reuse_both_standalone_reports_and_keep_each_original_context_once() {
    for case in [
        MaintenanceBlockingCase::Matched,
        MaintenanceBlockingCase::UnverifiedOriginal,
        MaintenanceBlockingCase::LaterDifferentOwner,
        MaintenanceBlockingCase::SamePidNewGeneration,
    ] {
        let (earlier, later) = fixtures(maintenance_blocking_evidence(case)).await;
        let reads = (earlier.recovery_object_reads(), later.recovery_object_reads());
        let progress = earlier.read_families(PROGRESS).await;
        let blocking = later.read_families(BLOCKING).await;
        let expected_progress = collected_resources::maintenance_progress(&progress).unwrap();
        let expected_blocking = collected_resources::blocking(&blocking).unwrap();
        let composed = collected_resources::maintenance_blocking(&progress, &blocking).unwrap();
        assert_eq!(
            serde_json::to_value(&composed.report.maintenance).unwrap(),
            serde_json::to_value(expected_progress.report).unwrap(),
            "{case:?}"
        );
        assert_eq!(
            serde_json::to_value(&composed.report.blocking).unwrap(),
            serde_json::to_value(expected_blocking.report).unwrap(),
            "{case:?}"
        );
        assert_eq!(composed.maintenance_context.source, *progress.source());
        assert_eq!(composed.blocking_context.source, *blocking.source());
        assert_eq!(composed.maintenance_context.capture, *progress.capture());
        assert_eq!(composed.blocking_context.capture, *blocking.capture());
        assert_ne!(composed.maintenance_context.capture, composed.blocking_context.capture);
        assert_eq!(composed.maintenance_context.records.len(), 1);
        assert_eq!(composed.blocking_context.records.len(), 2);
        assert_eq!(composed.report.original, composed.report.blocking.original);
        let record = &progress.records()[0];
        let expected = DiagnosticEvidenceRef::from_export(
            progress.source(),
            progress.capture_manifest_sha256(),
            record.row(),
            record.row_ordinal() - 1,
        )
        .unwrap();
        assert_eq!(composed.report.maintenance_observation, Some(expected));
        if case == MaintenanceBlockingCase::UnverifiedOriginal {
            assert_eq!(composed.report.maintenance.binding, FocusedRequestBinding::UnverifiedOriginalScope);
            assert_eq!(composed.report.blocking.binding, FocusedRequestBinding::UnverifiedOriginalScope);
        }
        if case == MaintenanceBlockingCase::LaterDifferentOwner {
            let historical = &composed.report.maintenance.entries[0].wait.as_ref().unwrap();
            assert_eq!(historical.unresolved_locker_pid.as_deref(), Some("43"));
            assert_eq!(composed.report.blocking.blocking.as_ref().unwrap().chains[0].tree.session, "44");
        }
        if case == MaintenanceBlockingCase::SamePidNewGeneration {
            let head = &composed.report.blocking.blocking.as_ref().unwrap().chains[0].tree;
            assert_eq!(head.session, "43");
            let Some(ActivityWaitOwner::Session { generation, .. }) = &head.owner else { panic!() };
            assert_eq!(generation.as_deref(), Some("2020-01-01T11:00:44Z"));
            let wait = serde_json::to_value(&composed.report.maintenance.entries[0].wait).unwrap();
            assert!(wait.get("generation").is_none(), "the earlier PID never gains this later generation");
        }
        assert_eq!((earlier.recovery_object_reads(), later.recovery_object_reads()), reads);
        assert_eq!(composed.maintenance_context.metrics.recovery_objects, 0);
        assert_eq!(composed.blocking_context.metrics.recovery_objects, 0);
        let value = serde_json::to_value(&composed.report).unwrap();
        for absent in ["snapshot", "workload", "records", "completed_at", "resolved", "historical_owner"] {
            assert!(value.get(absent).is_none(), "{absent}");
        }
    }
}

#[tokio::test]
async fn warehouse_omissions_remain_independent_and_do_not_rewrite_native_membership_or_operation_outcome() {
    for missing_progress in [true, false] {
        let (earlier, later) = fixtures(maintenance_blocking_evidence(MaintenanceBlockingCase::Matched)).await;
        if missing_progress {
            earlier.remove_first_row(TelemetryStream::OperationalSamples);
        } else {
            later.remove_first_row(TelemetryStream::ActivityObservations);
        }
        let progress = earlier.read_families(PROGRESS).await;
        let blocking = later.read_families(BLOCKING).await;
        let composed = collected_resources::maintenance_blocking(&progress, &blocking).unwrap();
        assert_eq!(composed.report.maintenance.read_complete, !missing_progress);
        assert_eq!(composed.report.blocking.read_complete, missing_progress);
        if missing_progress {
            assert!(composed.report.maintenance_observation.is_none());
            assert!(composed.report.maintenance.entries.is_empty());
            assert_eq!(composed.report.maintenance.native_rows, 1);
            assert_eq!(composed.report.maintenance.observed_rows, 0);
            assert_eq!(composed.report.maintenance.membership, FocusedRequestMembership::Observed);
            assert_eq!(composed.report.maintenance.source_state, SourceState::Partial);
            assert!(composed.report.blocking.blocking.is_some());
            assert!(
                composed.maintenance_context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows)
            );
            assert!(composed.blocking_context.read_gaps.is_empty());
        } else {
            assert!(composed.report.maintenance_observation.is_some());
            assert!(composed.report.blocking.blocking.is_none());
            assert_eq!(composed.report.blocking.native_rows, 2);
            assert_eq!(composed.report.blocking.observed_rows, 1);
            assert_eq!(composed.report.blocking.source_state, SourceState::Partial);
            assert!(composed.blocking_context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
            assert!(composed.maintenance_context.read_gaps.is_empty());
        }
    }
}

#[tokio::test]
async fn unavailable_and_limited_sources_keep_their_own_verdict_and_never_inherit_a_peer_read_success() {
    for case in [
        MaintenanceBlockingCase::ProgressUnavailable,
        MaintenanceBlockingCase::BlockingUnavailable,
        MaintenanceBlockingCase::BlockingLimited,
    ] {
        let (earlier, later) = fixtures(maintenance_blocking_evidence(case)).await;
        let progress = earlier.read_families(PROGRESS).await;
        let blocking = later.read_families(BLOCKING).await;
        let composed = collected_resources::maintenance_blocking(&progress, &blocking).unwrap();
        assert!(composed.report.maintenance.read_complete && composed.report.blocking.read_complete);
        match case {
            MaintenanceBlockingCase::ProgressUnavailable => {
                assert_eq!(composed.report.maintenance.source_state, SourceState::Unavailable);
                assert_eq!(composed.report.maintenance.membership, FocusedRequestMembership::Unavailable);
                assert!(composed.report.maintenance_observation.is_none());
                assert!(composed.report.blocking.blocking.is_some());
            }
            MaintenanceBlockingCase::BlockingUnavailable => {
                assert_eq!(composed.report.blocking.source_state, SourceState::Unavailable);
                assert_eq!(composed.report.blocking.membership, FocusedRequestMembership::Unavailable);
                assert!(composed.report.maintenance_observation.is_some());
                assert!(composed.report.blocking.blocking.is_none());
            }
            _ => {
                assert_eq!(composed.report.blocking.closure.status, BlockingClosureStatus::LookupLimit);
                assert_eq!(composed.report.blocking.source_state, SourceState::Partial);
                assert_eq!(composed.report.blocking.relationships.len(), 1);
                assert!(!composed.report.blocking.relationships[0].owner_observed);
            }
        }
    }
}

#[tokio::test]
async fn matching_display_ids_and_sql_never_link_foreign_originals_native_generations_or_reversed_native_time() {
    for case in [MaintenanceBlockingCase::MismatchedOriginal, MaintenanceBlockingCase::ChangedCurrentEpoch] {
        let (earlier, later) = fixtures(maintenance_blocking_evidence(case)).await;
        let progress = earlier.read_families(PROGRESS).await;
        let blocking = later.read_families(BLOCKING).await;
        let error = collected_resources::maintenance_blocking(&progress, &blocking).unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{case:?}");
    }
    let mut evidence = maintenance_blocking_evidence(MaintenanceBlockingCase::Matched);
    let SourceObservation::FocusedBlocking { snapshot, .. } = &mut evidence.blocking_source.observation else {
        panic!()
    };
    snapshot.observed_from = "2020-01-01T12:00:00.1Z".into();
    snapshot.observed_until = "2020-01-01T12:00:00.4Z".into();
    snapshot.at = "2020-01-01T12:00:00.2Z".into();
    let (earlier, later) = fixtures(evidence).await;
    let error = collected_resources::maintenance_blocking(
        &earlier.read_families(PROGRESS).await,
        &later.read_families(BLOCKING).await,
    )
    .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[tokio::test]
async fn absent_families_or_swapped_categories_do_not_trigger_an_alternate_read_or_analysis() {
    let (earlier, later) = fixtures(maintenance_blocking_evidence(MaintenanceBlockingCase::Matched)).await;
    let before = (earlier.recovery_object_reads(), later.recovery_object_reads());
    let progress = earlier.read_families(PROGRESS).await;
    let blocking = later.read_families(BLOCKING).await;
    assert!(collected_resources::maintenance_blocking(&blocking, &progress).is_err());
    assert!(collected_resources::maintenance_blocking(&earlier.read_families(BLOCKING).await, &blocking).is_err());
    assert!(collected_resources::maintenance_blocking(&progress, &later.read_families(PROGRESS).await).is_err());
    assert_eq!((earlier.recovery_object_reads(), later.recovery_object_reads()), before);
}
