//! Authenticated compiler context shares scan interpretation without manufacturing a workload or holder join.
use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::compiler_admission_evidence,
};
use culprit_core::{Snapshot, SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

fn native(source: &mut CollectionDescription) -> &mut StateSnapshotDescription {
    let SourceObservation::FocusedCompilationAdmission { snapshot, .. } = &mut source.observation else {
        panic!("focused compiler fixture")
    };
    snapshot
}

#[tokio::test]
async fn authenticated_compiler_context_reuses_scan_explanation_and_keeps_exact_raw_receipts_without_a_runtime_join() {
    let (source, records) = compiler_admission_evidence();
    let point = source.observation.snapshot().unwrap();
    let coverage = &point.sources[0];
    let observations = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = record else { panic!() };
            row.clone()
        })
        .collect::<Vec<_>>();
    let mut scan: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
    scan.server.database = source.observation.focus().unwrap().target.database().unwrap().database.clone();
    scan.operations.compiler_admission = Some(CompilerAdmissionSnapshot {
        source: coverage.source.clone(),
        context: Some(NativeReadContext::from_snapshot(&source).unwrap()),
        observations,
    });
    scan.sources.retain(|source| source.source != coverage.source);
    scan.sources.push(SourceOutcome {
        source: coverage.source.clone(),
        state: coverage.state,
        reason: None,
        reason_message: None,
    });
    let expected = culprit_checks::analyze(scan).compiler_admission.unwrap();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let result = collected_resources::compiler_admission(&capture).unwrap();
    assert_eq!(result.report, expected);
    assert_eq!(result.report.waiters.len(), 1);
    assert_eq!(result.report.gateways.len(), records.len() - 1);
    assert_eq!(result.context.source, source);
    assert_eq!(result.context.records.len(), records.len());
    assert_eq!(result.context.recovery_manifest_sha256, fixture.range.manifest_digest());
    for record in records {
        let expected = ExportedRecord::new(&source, capture.capture(), record).unwrap();
        assert!(result.context.records.iter().any(|record| record.row == expected));
    }
    assert_eq!(fixture.recovery_object_reads(), before);
    let json = serde_json::to_value(&result.report).unwrap();
    for absent in ["workload", "memory_total", "observations", "snapshot", "fix_sql"] {
        assert!(json.get(absent).is_none());
    }
}

#[tokio::test]
async fn missing_original_or_gateway_rows_remain_partial_and_do_not_complete_the_frozen_roster() {
    for missing_waiter in [true, false] {
        let (source, mut records) = compiler_admission_evidence();
        records.sort_by_key(|record| {
            let waiter = matches!(
                record,
                TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::CompilerWaiter(_), .. })
            );
            waiter != missing_waiter
        });
        let fixture = Fixture::new(source.clone(), records).await;
        fixture.remove_first_row(TelemetryStream::OperationalSamples);
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(!capture.is_read_complete());
        assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
        let result = collected_resources::compiler_admission(&capture).unwrap();
        assert_eq!(result.report.source_state, Some(SourceState::Partial));
        assert_eq!(result.report.waiters.len(), usize::from(!missing_waiter));
        assert_eq!(result.context.source, source, "the original source roster is never rewritten after an archive gap");
        assert_eq!(fixture.recovery_object_reads(), before);
    }
}

#[tokio::test]
async fn native_partial_empty_and_unavailable_outcomes_stay_distinct_from_a_complete_admission_read() {
    for state in [SourceState::Partial, SourceState::Empty, SourceState::Unavailable] {
        let (mut source, mut records) = compiler_admission_evidence();
        if state != SourceState::Partial {
            records.clear();
            let SourceObservation::FocusedCompilationAdmission { result, .. } = &mut source.observation else {
                panic!()
            };
            result.membership = CompilationAdmissionMembership::NotObserved;
            result.gateways.clear();
        }
        let native = native(&mut source);
        native.records = records.len() as u32;
        native.sources[0].records = records.len() as u32;
        native.sources[0].state = state;
        native.sources[0].complete_scope = state == SourceState::Empty;
        native.sources[0].error_code =
            (state == SourceState::Unavailable).then_some(culprit_core::ErrorCode::PermissionDenied);
        let fixture = Fixture::new(source, records).await;
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let result = collected_resources::compiler_admission(&capture).unwrap();
        assert_eq!(result.report.source_state, Some(state));
        if state != SourceState::Partial {
            assert!(result.report.waiters.is_empty());
        }
        assert!(!result.report.limitations.is_empty());
    }
}
