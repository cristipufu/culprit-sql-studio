use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{assert_operational_capture, configured_log_rate_evidence_cases},
};
use culprit_core::{Snapshot, SourceState, telemetry::*};

const FAMILIES: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn original_configuration_points_use_the_pure_view_without_extra_recovery_reads_or_losing_other_sources() {
    for (source, rows) in configured_log_rate_evidence_cases() {
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILIES).await;
        assert_operational_capture(&capture, &source, &rows);
        let analysis = culprit_engine::collected_resources::configured_log_rates(&capture).unwrap();
        let point = rows.iter().find_map(|row| match row {
            TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ConfiguredLogRateLimits(point),
                ..
            }) => Some(point.as_ref().clone()),
            _ => None,
        });
        let read = ConfiguredLogRateRead::from_source(&source, point).unwrap();
        let expected = culprit_checks::configured_log_rates::explain(&read, true).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(
            analysis.context.source.observation.snapshot().unwrap().sources.len(),
            source.observation.snapshot().unwrap().sources.len()
        );
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
        if let Some(point) = read.observation {
            assert_eq!(analysis.report.entries[0].bytes_per_second, point.workload_group_bytes_per_second);
            assert_eq!(analysis.report.entries[1].bytes_per_second, point.resource_pool_bytes_per_second);
            assert_eq!(analysis.report.entries[2].bytes_per_second, point.instance_bytes_per_second);
        }
    }
}

#[tokio::test]
async fn missing_configuration_body_retains_original_source_counts_and_never_becomes_healthy_emptiness() {
    let (source, rows) = configured_log_rate_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), rows).await;
    let opened = fixture.recovery_object_reads();
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILIES).await;
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = culprit_engine::collected_resources::configured_log_rates(&capture).unwrap();
    assert_eq!(analysis.report.native_rows, 1);
    assert_eq!(analysis.report.observed_rows, 0);
    assert!(analysis.report.entries.is_empty());
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert_eq!(analysis.context.source.observation.snapshot().unwrap().sources.len(), 2);
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[test]
fn report_display_omits_absent_configuration_and_preserves_the_present_exact_report() {
    let (source, mut rows) = configured_log_rate_evidence_cases().remove(0);
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::ConfiguredLogRateLimits(point),
        ..
    }) = rows.remove(0)
    else {
        panic!("the first shared case retains a native configuration point")
    };
    let read = ConfiguredLogRateRead::from_source(&source, Some(*point)).unwrap();
    let database = source.target.database().unwrap();
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.server.database = database.database.clone();
    snapshot.bind_identity(database.clone());
    snapshot.pressure.get_or_insert_with(Default::default).configured_log_rates = None;
    let absent = culprit_checks::analyze(snapshot.clone());
    assert!(absent.configured_log_rates.is_none());
    assert!(serde_json::to_value(&absent).unwrap().get("configured_log_rates").is_none());
    assert!(
        serde_json::to_value(culprit_engine::reports::display(&absent)).unwrap().get("configured_log_rates").is_none()
    );

    snapshot.pressure.as_mut().unwrap().configured_log_rates = Some(Box::new(read));
    let present = culprit_checks::analyze(snapshot);
    let expected = present.configured_log_rates.as_ref().unwrap();
    let display = serde_json::to_value(culprit_engine::reports::display(&present)).unwrap();
    assert_eq!(display["configured_log_rates"], serde_json::to_value(expected).unwrap());
    assert_eq!(display["configured_log_rates"]["entries"][0]["bytes_per_second"], "9007199254740993");
    assert_eq!(display["configured_log_rates"]["entries"][1]["bytes_per_second"], "0");
}
