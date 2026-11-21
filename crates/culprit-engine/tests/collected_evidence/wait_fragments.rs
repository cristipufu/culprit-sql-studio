use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{query_wait_evidence_cases, query_wait_fragment_evidence},
};
use culprit_core::telemetry::*;
#[tokio::test]
async fn original_wait_fragment_details_replay_from_the_canonical_family_without_provider_or_recovery_reads() {
    for (source, records) in query_wait_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::QueryWaits]).await;
        let report = culprit_engine::collected_resources::query_wait_fragments(&capture).unwrap();
        let expected = records
            .iter()
            .filter_map(|row| match row {
                TelemetryRecord::QueryWaits(wait) => Some(wait),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(report.entries.len(), expected.len());
        for (entry, wait) in report.entries.iter().zip(expected) {
            assert_eq!(entry.native, wait.key);
            assert_eq!(entry.outcome_description, wait.outcome_description);
            assert_eq!(entry.category_description, wait.category_description);
            assert_eq!(entry.unit, wait.unit);
        }
        assert_eq!(report.description.as_deref(), Some(&source));
        assert_eq!(report.capture, source.query_history().unwrap().waits.as_ref().map(|semantics| semantics.capture));
        assert_eq!(
            report.interval_coverage,
            source.query_history().unwrap().waits.as_ref().map(|semantics| semantics.coverage)
        );
        assert_eq!(fixture.recovery_object_reads(), before);
        assert_eq!(capture.metrics().recovery_objects, 0);
    }
}
#[tokio::test]
async fn missing_wait_body_keeps_original_selected_count_and_never_becomes_a_zero_wait_distribution() {
    let (source, records) = query_wait_fragment_evidence();
    let fixture = Fixture::new(source, records).await;
    fixture.remove_row(TelemetryStream::QueryWaits, 1);
    let capture = fixture.read_families(&[EvidenceFamily::QueryWaits]).await;
    assert!(!capture.is_read_complete());
    let report = culprit_engine::collected_resources::query_wait_fragments(&capture).unwrap();
    assert_eq!(report.selected_rows, Some(2));
    assert_eq!(report.observed_rows, 1);
    assert!(!report.read_complete);
    assert_eq!(report.source_state, culprit_core::SourceState::Partial);
}
#[tokio::test]
async fn wrong_native_family_cannot_supply_wait_fragment_identity_or_details() {
    let (source, records) = query_wait_fragment_evidence();
    let fixture = Fixture::new(source, records).await;
    let runtime = fixture.read_families(&[EvidenceFamily::QueryRuntime]).await;
    assert!(culprit_engine::collected_resources::query_wait_fragments(&runtime).is_err());
}
