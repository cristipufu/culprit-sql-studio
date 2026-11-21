use super::*;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{IncidentHistoryCase, incident_history_evidence},
};
use culprit_core::{IncidentHistoryProvenance, SourceState};
#[tokio::test]
async fn canonical_sdk_replays_selected_ongoing_uncertain_resolved_and_prefix_revisions_without_source_work() {
    for case in IncidentHistoryCase::ALL {
        let (source, rows) = incident_history_evidence(*case);
        let fixture = Fixture::new(source, rows).await;
        let original_reads = fixture.recovery_object_reads();
        let original = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let analysis = culprit_engine::collected_resources::incident_history(&original).unwrap();
        assert_eq!(analysis.report.provenance, IncidentHistoryProvenance::AuthenticatedCapture);
        assert_eq!(analysis.report.entries.len(), original.records().len());
        assert_eq!(analysis.report.source_state, Some(SourceState::Collected));
        assert!(analysis.report.entries.iter().all(|entry| entry.original.is_some()));
        assert_eq!(fixture.recovery_object_reads(), original_reads);
        assert_eq!(analysis.context.range_id, original.range_id());
    }
}
#[tokio::test]
async fn missing_historical_rows_stay_partial_and_cannot_recover_current_controller_state() {
    let (source, rows) = incident_history_evidence(IncidentHistoryCase::Resolved);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let original = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let analysis = culprit_engine::collected_resources::incident_history(&original).unwrap();
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert!(!analysis.report.selected_read_complete);
    assert!(!analysis.report.entries[0].predecessor_selected);
    assert!(analysis.report.limitations.contains(&culprit_core::t!("view.incident_history.limit.selection")));
}

#[tokio::test]
async fn an_authenticated_original_attempt_retains_its_recipe_outcome_and_native_settlement_as_history_only() {
    let (source, rows) = culprit_collector::analytical::test_support::shared_investigation_evidence();
    let fixture = Fixture::new(source, rows).await;
    let original = fixture
        .read_families_with_recovery(
            &[EvidenceFamily::OperationalSamples],
            culprit_collector::analytical::evidence::RecoveryPayloadBudget::default(),
        )
        .await;
    let analysis = culprit_engine::collected_resources::incident_history(&original).unwrap();
    assert_eq!(analysis.report.entries.len(), 1);
    let entry = &analysis.report.entries[0];
    assert!(entry.followup_recipe.is_some());
    assert!(entry.followup_recipe_version.is_some());
    assert!(entry.reasons.contains(&culprit_core::t!(
        "view.incident_history.investigation",
        outcome = "admitted",
        settlement = "not-started"
    )));
    assert!(analysis.report.limitations.contains(&culprit_core::t!("view.incident_history.limit.actions")));
}

#[tokio::test]
async fn shared_targets_and_distinct_native_epochs_keep_original_scope_without_database_relabeling() {
    for (source, rows) in culprit_collector::analytical::test_support::diagnostic_evidence_cases() {
        let fixture = Fixture::new(source.clone(), rows).await;
        let original = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let analysis = culprit_engine::collected_resources::incident_history(&original).unwrap();
        for entry in &analysis.report.entries {
            assert_eq!(entry.target_id, source.target.telemetry_id().unwrap());
            assert_eq!(entry.target_scope, source.target.measurement_scope());
        }
        if source.target.database().is_none() {
            assert!(analysis.report.entries.iter().all(|entry| entry.scope == culprit_core::MeasurementScope::Server));
        }
        if analysis.report.entries.len() == 2 {
            assert_ne!(analysis.report.entries[0].scope_id, analysis.report.entries[1].scope_id);
        }
    }
}
