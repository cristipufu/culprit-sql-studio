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

#[test]
fn canonical_tempdb_answer_text_keeps_observed_context_unresolved_causes_and_independent_clocks() {
    let (source, rows) = incident_history_evidence(IncidentHistoryCase::TemporaryResourcesPartial);
    let capture = CaptureIdentity {
        observation_id: "tempdb-history".into(),
        revision: Some(CaptureRevision { outbox_id: "owner".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let mut bytes = Vec::new();
    for row in rows.into_iter().filter(|row| row.stream() == TelemetryStream::OperationalSamples) {
        serde_json::to_writer(&mut bytes, &ExportedRecord::new(&source, &capture, row).unwrap()).unwrap();
        bytes.push(b'\n');
    }
    let guard = OperationGuard::new(culprit_engine::incident_history::HISTORY_IMPORT_TIMEOUT).unwrap();
    let report = culprit_engine::incident_history::import_reader(bytes.as_slice(), &guard.handle()).unwrap();
    let text = incident_history(&report);
    assert!(text.contains(&t!("view.tempdb_question.user", state = "observed")));
    assert!(text.contains(&t!("view.tempdb_question.spill")));
    assert!(text.contains(&t!("view.tempdb_question.coverage", state = "partial")));
    assert!(text.contains("2020-01-01T12:00:10Z"));
    assert_eq!(report.provenance, culprit_core::IncidentHistoryProvenance::ImportedArtifact);
}

#[test]
fn actual_artifact_intake_and_terminal_history_keep_original_compilation_context_and_competing_gaps() {
    use culprit_collector::analytical::test_support::{
        QueryCompilationFollowupCase, query_compilation_followup_evidence,
    };
    let (source, rows) = query_compilation_followup_evidence(QueryCompilationFollowupCase::Exact);
    let capture = CaptureIdentity {
        observation_id: "compilation-history".into(),
        revision: Some(CaptureRevision { outbox_id: "owner".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let mut bytes = vec![];
    for record in rows.into_iter().filter(|row| row.stream() == TelemetryStream::OperationalSamples) {
        serde_json::to_writer(&mut bytes, &ExportedRecord::new(&source, &capture, record).unwrap()).unwrap();
        bytes.push(b'\n');
    }
    let guard = OperationGuard::new(culprit_engine::incident_history::HISTORY_IMPORT_TIMEOUT).unwrap();
    let report = culprit_engine::incident_history::import_reader(bytes.as_slice(), &guard.handle()).unwrap();
    let text = incident_history(&report);
    assert!(text.contains("9007199254740993"));
    assert!(text.contains(&t!("view.incident_history.compilation.independent")));
    assert!(text.contains(&t!("view.incident_history.compilation.gap.work")));
    assert!(text.contains(&t!("view.incident_history.compilation.gap.context")));
}
