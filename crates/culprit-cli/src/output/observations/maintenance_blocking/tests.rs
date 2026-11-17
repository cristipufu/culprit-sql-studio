use super::*;
use culprit_checks::maintenance_blocking::{MaintenanceProgressEvidence, explain};
use culprit_collector::analytical::test_support::{MaintenanceBlockingCase, maintenance_blocking_evidence};
use culprit_core::telemetry::*;

// The native source relationships come from the shared corpus. These canonical refs exercise the pure report;
// the SDK replay tests separately authenticate the actual archive manifests and rows.
fn references(
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
    kind: &str,
    sequence: &str,
) -> Vec<DiagnosticEvidenceRef> {
    let capture = CaptureIdentity {
        observation_id: format!("cli-maintenance-blocking-{kind}"),
        revision: Some(CaptureRevision {
            outbox_id: "cli-renderer".into(),
            owner_epoch: "1".into(),
            sequence: sequence.into(),
        }),
    };
    let manifest = digest(&serde_json::to_vec(&(source, &capture, rows)).unwrap());
    rows.iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let row = ExportedRecord::new(source, &capture, row.clone()).unwrap();
            DiagnosticEvidenceRef::from_export(source, &manifest, &row, ordinal as u32).unwrap()
        })
        .collect()
}

fn report(
    case: MaintenanceBlockingCase,
    progress_complete: bool,
    blocking_complete: bool,
) -> MaintenanceBlockingReport {
    let mut fixture = maintenance_blocking_evidence(case);
    if !progress_complete {
        fixture.maintenance_rows.clear();
    }
    if !blocking_complete {
        fixture.blocking_rows.truncate(1);
    }
    let maintenance_refs = references(&fixture.maintenance_source, &fixture.maintenance_rows, "progress", "1");
    let blocking_refs = references(&fixture.blocking_source, &fixture.blocking_rows, "blocking", "2");
    let SourceObservation::MaintenanceProgress { snapshot, result } = &fixture.maintenance_source.observation else {
        panic!()
    };
    let progress = MaintenanceProgressSnapshot {
        source: snapshot.sources[0].source.clone(),
        context: NativeReadContext::from_snapshot(&fixture.maintenance_source).unwrap(),
        coverage: snapshot.sources[0].clone(),
        result: result.as_ref().clone(),
        observations: fixture
            .maintenance_rows
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::MaintenanceProgress(point),
                    ..
                }) = row
                else {
                    panic!()
                };
                point.as_ref().clone()
            })
            .collect(),
    };
    let rows = fixture
        .blocking_rows
        .iter()
        .zip(&blocking_refs)
        .map(|(row, reference)| {
            let TelemetryRecord::Activity(observation) = row else { panic!() };
            ActivityFrameRow { observation, reference: Some(reference) }
        })
        .collect::<Vec<_>>();
    let frame = ActivityFrameEvidence::new(&fixture.blocking_source, &rows, blocking_complete).unwrap();
    explain(
        MaintenanceProgressEvidence {
            source: &fixture.maintenance_source,
            snapshot: &progress,
            observation: maintenance_refs.first(),
            read_complete: progress_complete,
        },
        &frame,
    )
    .unwrap()
}

#[test]
fn text_separates_historical_native_phase_from_later_relationships_and_keeps_both_receipts_and_bounds() {
    let report = report(MaintenanceBlockingCase::LaterDifferentOwner, true, true);
    let text = maintenance_blocking(&report);
    let historical = t!("evidence.cli.maintenance_blocking.historical");
    let later = t!("evidence.cli.maintenance_blocking.later");
    assert!(text.find(&historical).unwrap() < text.find(&later).unwrap());
    assert!(text.contains(&report.summary));
    assert!(text.contains(&report.maintenance.context.observed_from));
    assert!(text.contains(&report.maintenance.context.observed_until));
    assert!(text.contains(&report.blocking.context.observed_from));
    assert!(text.contains(&report.blocking.context.observed_until));
    assert!(text.contains(&report.maintenance_observation.as_ref().unwrap().record_id));
    assert!(text.contains(&report.original.record_id));
    assert!(text.contains(&report.maintenance.entries[0].phase));
    assert!(text.contains(&report.maintenance.entries[0].explanation));
    assert!(text.contains(&report.maintenance.entries[0].investigation));
    assert!(text.contains(&report.maintenance.entries[0].verify));
    assert!(text.contains(&super::super::blocking(&report.blocking)));
    assert!(text.len() < 32 * 1024);
}

#[test]
fn missing_archived_progress_stays_an_explicit_read_gap_without_reconstructing_its_phase_or_receipt() {
    let report = report(MaintenanceBlockingCase::Matched, false, true);
    assert!(report.maintenance_observation.is_none());
    let text = maintenance_blocking(&report);
    assert!(text.contains(&t!("evidence.cli.read_incomplete")));
    assert!(text.contains(&t!("evidence.cli.maintenance_blocking.progress_counts", native = "1", observed = "0")));
    assert!(!text.contains("waiting for writers before build"));
    assert!(!text.contains(&t!("evidence.cli.maintenance_blocking.progress_receipt", record = "")));
    assert!(text.contains(&super::super::blocking(&report.blocking)));
}

#[test]
fn independent_native_unavailability_original_scope_and_partial_blocker_rows_remain_visible() {
    for case in [MaintenanceBlockingCase::ProgressUnavailable, MaintenanceBlockingCase::BlockingUnavailable] {
        let report = report(case, true, true);
        let text = maintenance_blocking(&report);
        assert!(text.contains(&t!("evidence.cli.source_unavailable")));
        assert!(text.contains(&report.summary));
    }
    let unverified = report(MaintenanceBlockingCase::UnverifiedOriginal, true, true);
    assert_eq!(unverified.blocking.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    assert!(maintenance_blocking(&unverified).contains(&t!("view.blocking.limit.original_scope")));
    let partial = report(MaintenanceBlockingCase::Matched, true, false);
    assert!(!partial.blocking.read_complete);
    assert!(maintenance_blocking(&partial).contains(&t!("view.blocking.summary.read_gap")));
    assert!(maintenance_blocking(&partial).contains(&super::super::blocking(&partial.blocking)));
}

#[test]
fn text_preserves_backend_formatted_exact_values_and_bounds_long_presentation_without_inferring_from_them() {
    let mut report = report(MaintenanceBlockingCase::Matched, true, true);
    // Presentation-only expansion: diagnosis and native relationships were produced by the shared view above.
    assert!(maintenance_blocking(&report).contains("9007199254740993"));
    report.maintenance.entries[0].explanation = "é".repeat(4096);
    report.limitations = (0..100).map(|_| "λ".repeat(4096)).collect();
    let text = maintenance_blocking(&report);
    assert!(text.contains("9007199254740993"));
    assert!(!text.contains("9,007,199,254,740,993"));
    assert!(!text.contains(&"é".repeat(4096)));
    assert!(
        text.chars().filter(|character| *character == 'λ').count()
            <= MAX_COMPOSITION_LIMITATIONS * OBSERVATION_DETAIL_CHARS
    );
    assert!(text.contains(&t!("evidence.cli.maintenance_blocking.later")));
    assert!(text.len() < 32 * 1024);
}
