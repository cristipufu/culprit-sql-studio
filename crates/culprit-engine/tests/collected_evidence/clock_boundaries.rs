use super::fixture::Fixture;
use culprit_collector::analytical::test_support::{assert_query_cohort_capture, page_server_read_evidence_cases};
use culprit_core::{ErrorDetail, collected_analysis::CollectedAnalysisLimit, telemetry::*};
use culprit_engine::collected_evidence;

const COLLECTED_FROM: &str = "2020-01-01T06:59:54.123456789Z";
const COLLECTED_UNTIL: &str = "2020-01-01T06:59:55.123456790Z";
const NATIVE_FROM: &str = "2020-01-01T06:00:00.000000001Z";
const NATIVE_UNTIL: &str = "2020-01-01T07:00:00.000000002Z";
const FIRST_EXECUTION: &str = "2020-01-01T06:59:56.123456700Z";
const LAST_EXECUTION: &str = "2020-01-01T06:59:59.123456800Z";

fn original() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let case = page_server_read_evidence_cases()
        .into_iter()
        .find(|case| case.name == "single_fragment_with_definitions")
        .unwrap();
    let mut source = case.source;
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
    history.interval.from = NATIVE_FROM.into();
    history.interval.until = NATIVE_UNTIL.into();
    let mut records = case.records;
    let native = runtime(&mut records);
    native.first_execution_at = Some(FIRST_EXECUTION.into());
    native.last_execution_at = Some(LAST_EXECUTION.into());
    (source, records)
}
fn runtime(records: &mut [TelemetryRecord]) -> &mut RuntimeMeasurement {
    records
        .iter_mut()
        .find_map(|row| if let TelemetryRecord::QueryRuntime(row) = row { Some(row) } else { None })
        .unwrap()
}

#[tokio::test]
async fn authenticated_native_window_ahead_of_collector_time_preserves_both_original_clocks_and_proofs() {
    let (source, records) = original();
    let fixture =
        Fixture::with_collector_bounds(source.clone(), records.clone(), COLLECTED_FROM, COLLECTED_UNTIL).await.unwrap();
    let opens = fixture.recovery_object_reads();
    let capture = fixture.read().await;
    assert_query_cohort_capture(&capture, &source, &records);
    assert_eq!(capture.collected_from(), COLLECTED_FROM);
    assert_eq!(capture.collected_until(), COLLECTED_UNTIL);
    let analysis = collected_evidence::analyze(&capture, Some(&NativeIdentity::new("1"))).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.collected_from, COLLECTED_FROM);
    assert_eq!(analysis.context.collected_until, COLLECTED_UNTIL);
    assert_eq!(analysis.context.recovery_manifest_sha256, fixture.range.manifest_digest());
    assert_eq!(analysis.context.capture_manifest_sha256, capture.capture_manifest_sha256());
    let workload = analysis.analyses[0].workload.as_ref().unwrap();
    assert_eq!(workload.window_start.as_deref(), Some(NATIVE_FROM));
    assert_eq!(workload.window_end.as_deref(), Some(NATIVE_UNTIL));
    assert_eq!(workload.total_executions, 1.0);
    assert_eq!(workload.queries[0].first_seen.as_deref(), Some(FIRST_EXECUTION));
    assert_eq!(workload.queries[0].last_seen.as_deref(), Some(LAST_EXECUTION));
    assert!(
        !analysis.analyses[0].limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::RuntimeBounds)
    );
    for record in &analysis.context.records {
        let exported = ExportedRecord::new(&source, capture.capture(), record.row.record.clone()).unwrap();
        assert_eq!(record.row, exported);
        assert_eq!(record.native_payload_sha256, exported.payload_sha256().unwrap());
    }
    assert_eq!(
        fixture.recovery_object_reads(),
        opens,
        "clock independence needs no recovery payload or source follow-up"
    );
}

#[tokio::test]
async fn reversed_or_empty_native_windows_are_rejected_even_when_the_collector_bounds_are_valid() {
    for (from, until) in [(NATIVE_UNTIL, NATIVE_FROM), (NATIVE_UNTIL, NATIVE_UNTIL)] {
        let (mut source, records) = original();
        let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
        history.interval.from = from.into();
        history.interval.until = until.into();
        let fixture = Fixture::with_collector_bounds(source.clone(), records.clone(), COLLECTED_FROM, COLLECTED_UNTIL)
            .await
            .unwrap();
        let capture = fixture.read().await;
        assert_query_cohort_capture(&capture, &source, &records);
        let error = collected_evidence::analyze(&capture, Some(&NativeIdentity::new("1"))).unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}

#[tokio::test]
async fn reversed_or_invalid_collector_bounds_cannot_borrow_the_valid_native_window() {
    let (source, records) = original();
    let fixture =
        Fixture::with_collector_bounds(source.clone(), records.clone(), COLLECTED_UNTIL, COLLECTED_FROM).await.unwrap();
    let capture = fixture.read().await;
    assert_query_cohort_capture(&capture, &source, &records);
    let error = collected_evidence::analyze(&capture, Some(&NativeIdentity::new("1"))).unwrap_err();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    let error = Fixture::with_collector_bounds(source.clone(), records.clone(), "not-a-clock", COLLECTED_UNTIL)
        .await
        .err()
        .expect("invalid original collector clock must not seal");
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[test]
fn invalid_native_clock_shapes_cannot_acquire_an_original_export_proof() {
    for invalid in ["not-a-clock", "2020-01-01T06:00:00", "2020-01-01T06:00:00.1234567891Z"] {
        let (mut source, records) = original();
        let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!() };
        history.interval.from = invalid.into();
        assert!(source.captured_range().is_err());
        let capture = CaptureIdentity {
            observation_id: "a".repeat(32),
            revision: Some(CaptureRevision {
                outbox_id: "b".repeat(32),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        assert!(ExportedRecord::new(&source, &capture, records[0].clone()).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn future_or_reversed_native_execution_times_still_withhold_the_runtime_projection() {
    for (first, last) in [(FIRST_EXECUTION, "2020-01-01T07:00:00.000000003Z"), (LAST_EXECUTION, FIRST_EXECUTION)] {
        let (source, mut records) = original();
        let native = runtime(&mut records);
        native.first_execution_at = Some(first.into());
        native.last_execution_at = Some(last.into());
        let fixture = Fixture::with_collector_bounds(source.clone(), records.clone(), COLLECTED_FROM, COLLECTED_UNTIL)
            .await
            .unwrap();
        let capture = fixture.read().await;
        assert_query_cohort_capture(&capture, &source, &records);
        let analysis = collected_evidence::analyze(&capture, Some(&NativeIdentity::new("1"))).unwrap();
        assert!(analysis.analyses[0].workload.is_none());
        assert!(
            analysis.analyses[0].limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::RuntimeBounds)
        );
        assert_eq!(analysis.context.records.len(), records.len(), "raw originals survive a refused projection");
    }
}
