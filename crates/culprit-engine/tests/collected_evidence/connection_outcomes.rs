use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{assert_connection_outcome_capture, connection_outcome_evidence_cases},
};
use culprit_core::{
    ConnectionOutcomeAvailability, ConnectionOutcomeMetricKind, ConnectionOutcomeSnapshot, SourceState, telemetry::*,
};

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn authenticated_archive_outcomes_use_the_same_pure_view_with_all_original_source_rows_and_no_new_io() {
    for (source, records) in connection_outcome_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert_connection_outcome_capture(&capture, &source, &records);
        let rows = records
            .into_iter()
            .map(|record| {
                let TelemetryRecord::Operational(row) = record else { panic!("native counter set") };
                row
            })
            .collect();
        let raw = ConnectionOutcomeSnapshot::from_source(&source, rows).unwrap();
        let expected = culprit_checks::connection_outcomes::explain(&raw, true).unwrap();
        let actual = culprit_engine::collected_resources::connection_outcomes(&capture).unwrap();
        assert_eq!(serde_json::to_value(actual.report.as_ref()).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(actual.context.source, source);
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}

#[tokio::test]
async fn native_counts_zero_null_absent_layout_and_mysql_attempt_semantics_stay_distinct() {
    let cases = connection_outcome_evidence_cases();
    let (source, records) = cases[0].clone();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(FAMILY).await;
    let analysis = culprit_engine::collected_resources::connection_outcomes(&capture).unwrap();
    assert_eq!((analysis.report.native_rows, analysis.report.observed_rows, analysis.report.mapped_rows), (2, 2, 1));
    let metric = |kind| analysis.report.entries[0].metrics.iter().find(|metric| metric.kind == kind).unwrap();
    assert_eq!(
        metric(ConnectionOutcomeMetricKind::SessionsEstablished).value,
        Some(MeasurementValue::Integer("9007199254740993".into()))
    );
    assert_eq!(
        metric(ConnectionOutcomeMetricKind::SessionsLostClient).value,
        Some(MeasurementValue::Integer("0".into()))
    );
    assert!(metric(ConnectionOutcomeMetricKind::SessionsFatal).value.is_none());
    for (index, expected) in
        [(2, ConnectionOutcomeAvailability::Unsupported), (3, ConnectionOutcomeAvailability::Supported)]
    {
        let (source, records) = cases[index].clone();
        let fixture = Fixture::new(source, records).await;
        let analysis =
            culprit_engine::collected_resources::connection_outcomes(&fixture.read_families(FAMILY).await).unwrap();
        assert_eq!(analysis.report.entries[0].metrics[0].availability, expected);
    }
    let (source, records) = cases[6].clone();
    let fixture = Fixture::new(source, records).await;
    let analysis =
        culprit_engine::collected_resources::connection_outcomes(&fixture.read_families(FAMILY).await).unwrap();
    assert_eq!((analysis.report.native_rows, analysis.report.observed_rows, analysis.report.mapped_rows), (13, 13, 12));
    assert!(analysis.report.entries.iter().all(|entry| entry.scope == culprit_core::MeasurementScope::Server));
    assert!(analysis.report.entries.iter().flat_map(|entry| &entry.metrics).all(|metric| metric.rate.is_none()));
    assert!(
        analysis
            .report
            .entries
            .iter()
            .flat_map(|entry| &entry.metrics)
            .any(|metric| metric.kind == ConnectionOutcomeMetricKind::ConnectionAttempts
                && metric.value == Some(MeasurementValue::Integer("18446744073709551615".into())))
    );
}

#[tokio::test]
async fn missing_or_unpublished_evidence_keeps_original_counts_and_cannot_become_a_healthy_empty_report() {
    let (source, records) = connection_outcome_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery_object_reads();
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILY).await;
    assert!(!capture.is_read_complete());
    let analysis = culprit_engine::collected_resources::connection_outcomes(&capture).unwrap();
    assert_eq!(analysis.report.native_rows, records.len() as u32);
    assert_eq!(analysis.report.observed_rows, records.len() as u32 - 1);
    assert!(analysis.report.sources.iter().any(|source| source.state == SourceState::Partial));
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    fixture.unpublish();
    let capture = fixture.read_families(FAMILY).await;
    let analysis = culprit_engine::collected_resources::connection_outcomes(&capture).unwrap();
    assert!(analysis.report.entries.is_empty());
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_rows, records.len() as u32);
    assert!(analysis.report.sources.iter().all(|source| source.state == SourceState::Partial));
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[tokio::test]
async fn another_valid_operational_fact_shape_is_not_reinterpreted_as_connection_evidence() {
    let (source, records) = culprit_collector::analytical::test_support::file_io_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(FAMILY).await;
    let error = culprit_engine::collected_resources::connection_outcomes(&capture).unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)));
}
