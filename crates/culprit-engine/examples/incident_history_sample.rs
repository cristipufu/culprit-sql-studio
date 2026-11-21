//! The demo and UI controls consume the same structural artifact reader as the real desktop workflow.
use culprit_collector::analytical::test_support::{
    IncidentHistoryCase, QueryCompilationFollowupCase, incident_history_evidence, query_compilation_followup_evidence,
};
use culprit_core::{limits::OperationGuard, telemetry::*};
use std::{collections::BTreeMap, path::PathBuf};
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    culprit_core::i18n::scope("en", generate)
}
fn generate() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut cases = BTreeMap::new();
    let originals = [
        ("exact", IncidentHistoryCase::Many),
        ("resolved", IncidentHistoryCase::Resolved),
        ("ongoing", IncidentHistoryCase::Ongoing),
        ("uncertain", IncidentHistoryCase::Uncertain),
        ("prefix", IncidentHistoryCase::Prefix),
        ("temporary", IncidentHistoryCase::TemporaryResources),
        ("temporary_partial", IncidentHistoryCase::TemporaryResourcesPartial),
    ]
    .into_iter()
    .map(|(name, case)| (name, incident_history_evidence(case)))
    .chain(
        [
            ("compilation", QueryCompilationFollowupCase::Exact),
            ("compilation_partial", QueryCompilationFollowupCase::Partial),
            ("compilation_changed", QueryCompilationFollowupCase::Changed),
            ("compilation_unavailable", QueryCompilationFollowupCase::Unavailable),
        ]
        .into_iter()
        .map(|(name, case)| (name, query_compilation_followup_evidence(case))),
    );
    for (name, (source, rows)) in originals {
        let capture = CaptureIdentity {
            observation_id: "controlled-history-artifact".into(),
            revision: Some(CaptureRevision {
                outbox_id: "owner".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let mut bytes = vec![];
        for record in rows.into_iter().filter(|row| row.stream() == TelemetryStream::OperationalSamples) {
            serde_json::to_writer(&mut bytes, &ExportedRecord::new(&source, &capture, record)?)?;
            bytes.push(b'\n');
        }
        let guard = OperationGuard::new(culprit_engine::incident_history::HISTORY_IMPORT_TIMEOUT)?;
        cases.insert(name, culprit_engine::incident_history::import_reader(bytes.as_slice(), &guard.handle())?);
    }
    let path = std::env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/incident-history.sample.json")
    });
    std::fs::write(path, serde_json::to_string_pretty(&cases)? + "\n")?;
    Ok(())
}
