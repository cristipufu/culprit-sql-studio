use super::*;
use culprit_collector::analytical::test_support::{IncidentHistoryCase, incident_history_evidence};
use culprit_core::{limits::OperationGuard, telemetry::*};
fn report() -> IncidentHistoryReport {
    let (source, rows) = incident_history_evidence(IncidentHistoryCase::Many);
    let capture = CaptureIdentity {
        observation_id: "history".into(),
        revision: Some(CaptureRevision { outbox_id: "owner".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let bytes = rows
        .into_iter()
        .filter(|row| row.stream() == TelemetryStream::OperationalSamples)
        .flat_map(|row| {
            let mut bytes = serde_json::to_vec(&ExportedRecord::new(&source, &capture, row).unwrap()).unwrap();
            bytes.push(b'\n');
            bytes
        })
        .collect::<Vec<_>>();
    let guard = OperationGuard::new(culprit_engine::incident_history::HISTORY_IMPORT_TIMEOUT).unwrap();
    culprit_engine::incident_history::import_reader(bytes.as_slice(), &guard.handle()).unwrap()
}
#[test]
fn selected_history_text_has_qualified_phases_clocks_evidence_and_a_bounded_escaped_population() {
    let mut report = report();
    report.entries[0].source = "source\n\u{1b}[31m".into();
    let text = incident_history(&report);
    assert!(text.contains(&report.entries[0].phase));
    assert!(text.contains(&report.entries[0].first_seen_at));
    assert!(text.contains("source\\n\\u{1b}"));
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains(&t!("view.incident_history.cli.omitted", shown = "10", total = "13")));
    assert!(!text.contains(&report.entries[10].title));
    assert!(text.contains(&t!("view.incident_history.limit.imported")));
}
