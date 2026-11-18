use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, DatabasePropertiesCase},
};
use culprit_core::{SourceState, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn database_properties_replay_keeps_original_native_namespace_codes_unknowns_and_pure_meanings() {
    for case in DatabasePropertiesCase::ALL {
        let (source, rows) = test_support::database_properties_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let analysis = collected_resources::database_properties(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.native_source.as_ref(), Some(&source.observation.snapshot().unwrap().sources[0]));
        assert_eq!(analysis.report.read_complete, Some(true));
        let read = DatabasePropertiesRead::from_source(
            &source,
            rows.into_iter()
                .map(|row| {
                    let TelemetryRecord::Operational(row) = row else { panic!() };
                    row
                })
                .collect(),
            true,
        )
        .unwrap();
        let expected = culprit_checks::database_properties::explain(&read).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(fixture.recovery_object_reads(), opened);
        if case == DatabasePropertiesCase::Exact {
            assert_eq!(analysis.report.fields.len(), 16);
            assert_eq!(analysis.report.native_database_id.as_deref(), Some("5"));
            assert_eq!(
                analysis
                    .report
                    .fields
                    .iter()
                    .find(|field| field.key == "target_recovery_time_seconds")
                    .unwrap()
                    .value
                    .as_deref(),
                Some("0")
            );
        }
        if case == DatabasePropertiesCase::UnknownCodes {
            let state = analysis.report.fields.iter().find(|field| field.key == "catalog_state").unwrap();
            assert!(state.value.is_none());
            assert_eq!(state.native_code.as_deref(), Some("255"));
            assert_eq!(state.native_description.as_deref(), Some("ONLINE"));
        }
    }
}

#[tokio::test]
async fn missing_property_body_preserves_original_source_count_and_partial_read_without_hydration() {
    let (source, rows) = test_support::database_properties_evidence(DatabasePropertiesCase::Exact);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = collected_resources::database_properties(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_source.as_ref().unwrap().records, 1);
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert_eq!(analysis.report.read_complete, Some(false));
    assert!(analysis.report.fields.is_empty());
    assert!(analysis.report.observed_at.is_none());
    assert_eq!(fixture.recovery_object_reads(), opened);
}
