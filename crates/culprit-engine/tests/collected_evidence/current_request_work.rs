use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason, RecoveryPayloadBudget},
    test_support::current_request_work_evidence_cases,
};
use culprit_core::{SourceState, telemetry::*};
use culprit_engine::collected_resources;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

fn raw(source: &CollectionDescription, records: &[TelemetryRecord]) -> CurrentRequestWorkSnapshot {
    let SourceObservation::CurrentRequestWork { snapshot, result } = &source.observation else { panic!() };
    CurrentRequestWorkSnapshot {
        source: source.source.clone(),
        context: result.read_context(source, snapshot),
        coverage: snapshot.sources[0].clone(),
        result: result.as_ref().clone(),
        observations: records
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::CurrentRequestWork(point),
                    ..
                }) = record
                else {
                    panic!()
                };
                point.as_ref().clone()
            })
            .collect(),
    }
}

#[tokio::test]
async fn captured_native_work_reuses_the_same_pure_report_and_retains_raw_context_once() {
    let cases = current_request_work_evidence_cases();
    assert_eq!(cases.len(), 13);
    for (source, records) in cases.into_iter().take(12) {
        let expected = culprit_checks::current_request_work::explain(&raw(&source, &records), true).unwrap();
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opens = fixture.recovery_object_reads();
        let capture = fixture.read_families(FAMILY).await;
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        let analysis = collected_resources::current_request_work(&capture).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), records.len());
        assert!(analysis.context.records.iter().zip(&records).all(|(actual, expected)| actual.row.record == *expected));
        assert_eq!(analysis.context.metrics.recovery_objects, 0);
        assert_eq!(fixture.recovery_object_reads(), opens);
        if let Some(entry) = analysis.report.entries.first() {
            for (key, value) in [
                ("rows_examined", "9007199254740993"),
                ("timer_wait_ps", "18446744073709551615"),
                ("created_tmp_disk_tables", "0"),
            ] {
                let key = format!("view.current_request_work.metric.{key}");
                assert_eq!(entry.metrics.iter().find(|metric| metric.is(&key)).unwrap().text.as_deref(), Some(value));
            }
        }
        let report = serde_json::to_value(&analysis.report).unwrap();
        for field in ["snapshot", "workload", "sql", "plan", "percent", "spill_bytes"] {
            assert!(report.get(field).is_none(), "{field}");
        }
        for private in ["private-current-work-client", "private-current-work-login", "private-current-work-sql"] {
            assert!(!report.to_string().contains(private));
        }
    }
}

#[tokio::test]
async fn absent_warehouse_rows_keep_original_positive_membership_without_promoting_complete_empty() {
    let (source, records) = current_request_work_evidence_cases().remove(0);
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture = fixture.read_families(FAMILY).await;
    let analysis = collected_resources::current_request_work(&capture).unwrap();
    assert!(!capture.is_read_complete());
    assert!(analysis.report.entries.is_empty());
    assert_eq!(analysis.report.membership, FocusedRequestMembership::Observed);
    assert_eq!(analysis.report.native_rows, 1);
    assert_eq!(analysis.report.observed_rows, 0);
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert_eq!(analysis.context.source, source);
    assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
}

#[tokio::test]
async fn missing_and_changed_requests_or_denied_source_remain_native_unknowns_after_complete_archive_read() {
    for (source, records) in current_request_work_evidence_cases().into_iter().filter(|(_, records)| records.is_empty())
    {
        let SourceObservation::CurrentRequestWork { snapshot, result } = &source.observation else { panic!() };
        let fixture = Fixture::new(source.clone(), records).await;
        let capture = fixture.read_families(FAMILY).await;
        let analysis = collected_resources::current_request_work(&capture).unwrap();
        assert!(capture.is_read_complete());
        assert!(analysis.report.entries.is_empty());
        assert_eq!(analysis.report.native_rows, 0);
        assert_eq!(analysis.report.observed_rows, 0);
        assert_eq!(analysis.report.membership, result.membership);
        assert_eq!(analysis.report.source_state, snapshot.sources[0].state);
        assert_ne!(analysis.report.source_state, SourceState::Empty);
    }
}

#[tokio::test]
async fn preview_only_current_sql_is_a_read_gap_until_explicit_authenticated_hydration() {
    let (source, records) = current_request_work_evidence_cases().pop().unwrap();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery_object_reads();
    let preview = fixture.read_families(FAMILY).await;
    let incomplete = collected_resources::current_request_work(&preview).unwrap();
    assert!(incomplete.report.entries.is_empty());
    assert_eq!(incomplete.report.source_state, SourceState::Partial);
    assert_eq!(incomplete.report.native_rows, 1);
    assert_eq!(incomplete.report.observed_rows, 0);
    assert_eq!(incomplete.report.membership, FocusedRequestMembership::Observed);
    assert!(preview.gaps().iter().any(|gap| matches!(
        gap.reason,
        EvidenceGapReason::DetailInRecovery | EvidenceGapReason::NativeRepresentationRequired
    )));
    assert_eq!(fixture.recovery_object_reads(), opens);
    let hydrated = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    let analysis = collected_resources::current_request_work(&hydrated).unwrap();
    let expected = culprit_checks::current_request_work::explain(&raw(&source, &records), true).unwrap();
    assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
    assert_eq!(analysis.context.source, source);
    assert_eq!(hydrated.metrics().recovery_objects, 1);
    assert_eq!(analysis.context.records[0].row.record, records[0]);
    assert_eq!(fixture.recovery_object_reads() - opens, 1);
}

#[tokio::test]
async fn missing_warehouse_current_work_is_never_recovered_from_an_unrequested_native_payload() {
    let (source, records) = current_request_work_evidence_cases().pop().unwrap();
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let opens = fixture.recovery_object_reads();
    let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
    let analysis = collected_resources::current_request_work(&capture).unwrap();
    assert!(analysis.report.entries.is_empty());
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert_eq!(analysis.context.source, source);
    assert_eq!(fixture.recovery_object_reads(), opens);
    assert_eq!(analysis.context.read_gaps[0].reason, EvidenceGapReason::MissingRows);
}

#[tokio::test]
async fn wrong_requested_family_cannot_supply_an_empty_current_request_report() {
    let (source, records) = current_request_work_evidence_cases().remove(0);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    assert!(collected_resources::current_request_work(&capture).is_err());
}

#[test]
fn future_or_recycled_native_points_cannot_be_sealed_under_the_original_header() {
    let (source, records) = current_request_work_evidence_cases().remove(0);
    let capture = CaptureIdentity { observation_id: "invalid-current-work-point".into(), revision: None };
    for (field, value) in [("clock", "2021-01-01T00:00:00Z"), ("thread", "108"), ("event", "99")] {
        let mut record = records[0].clone();
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::CurrentRequestWork(point),
            ..
        }) = &mut record
        else {
            panic!()
        };
        match field {
            "clock" => point.observed_at = value.into(),
            "thread" => point.native.thread_id = value.into(),
            _ => point.native.statement_event_id = value.into(),
        };
        assert!(ExportedRecord::new(&source, &capture, record).is_err(), "{field}");
    }
}
