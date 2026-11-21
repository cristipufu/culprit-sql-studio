use super::*;
use culprit_core::{
    IncidentHistoryEntry, IncidentHistoryEvidence, IncidentHistoryProvenance, IncidentHistoryReport, MeasurementScope,
    telemetry::{
        CaptureIdentity, CaptureRevision, DiagnosticAnalysisStatus, DiagnosticCaptureRef, DiagnosticLifecycle, digest,
    },
};
fn report() -> IncidentHistoryReport {
    let entry = IncidentHistoryEntry {
        controller_version: 13,
        revision_id: digest(b"revision"),
        episode_id: digest(b"episode"),
        target_id: digest(b"target"),
        target_scope: MeasurementScope::Server,
        scope_id: digest(b"scope"),
        source: "private-source-not-projected".into(),
        scope: MeasurementScope::Server,
        scope_label: "controlled shared scope".into(),
        revision: "9007199254740993".into(),
        previous_revision: Some("9007199254740992".into()),
        predecessor_selected: false,
        lifecycle: DiagnosticLifecycle::Resolved,
        analysis: DiagnosticAnalysisStatus::Unresolved,
        title: "controlled title".into(),
        phase: "controlled phase".into(),
        first_seen_at: "2020-01-01T12:00:00Z".into(),
        last_seen_at: "2020-01-01T12:00:01Z".into(),
        recorded_at: "2020-01-01T12:01:00Z".into(),
        original: None,
        input_references: 3,
        followup_recipe: None,
        followup_recipe_version: None,
        evidence: vec![IncidentHistoryEvidence {
            label: "controlled reference".into(),
            capture: DiagnosticCaptureRef {
                capture: CaptureIdentity {
                    observation_id: "private-observation".into(),
                    revision: Some(CaptureRevision {
                        outbox_id: "owner".into(),
                        owner_epoch: "1".into(),
                        sequence: "1".into(),
                    }),
                },
                manifest_sha256: digest(b"manifest"),
                source_scope_id: digest(b"scope"),
            },
            reference: None,
            availability: "reference-only".into(),
        }],
        issue: None,
        reasons: vec!["controlled reason".into()],
        next_step: "controlled next step".into(),
    };
    IncidentHistoryReport {
        provenance: IncidentHistoryProvenance::ImportedArtifact,
        selected_read_complete: false,
        source_state: None,
        source_reason: None,
        title: "controlled title".into(),
        summary: "controlled summary".into(),
        limitations: vec![],
        entries: vec![entry; ENTRY_LIMIT + 2],
    }
}
#[test]
fn compact_history_preserves_exact_revision_null_unknown_scope_and_omission_without_private_source_or_actions() {
    let value = value(&report(), 42).unwrap();
    let history = &value["incident_history"];
    assert_eq!(history["entries_presented"], 5);
    assert_eq!(history["entries_omitted"], 2);
    assert_eq!(history["entries"][0]["revision"], "9007199254740993");
    assert!(history["source_state"].is_null());
    assert_eq!(history["entries"][0]["lifecycle"], "resolved");
    assert_eq!(history["entries"][0]["analysis"], "unresolved");
    assert!(history["entries"][0]["original"].is_null());
    assert!(history["entries"][0].get("actions").is_none());
    let json = serde_json::to_string(&value).unwrap();
    assert!(!json.contains("private-source"));
    assert!(!json.contains("private-observation"));
}
#[test]
fn malformed_reference_does_not_get_a_fabricated_hash_or_empty_success() {
    let mut report = report();
    report.entries[0].evidence[0].capture.manifest_sha256.clear();
    assert!(value(&report, 0).is_err());
}

#[test]
fn canonical_tempdb_answer_survives_the_existing_compact_history_projection_without_native_authority() {
    use culprit_collector::analytical::test_support::{IncidentHistoryCase, incident_history_evidence};
    let (source, rows) = incident_history_evidence(IncidentHistoryCase::TemporaryResources);
    let capture = CaptureIdentity {
        observation_id: "tempdb-history".into(),
        revision: Some(CaptureRevision { outbox_id: "owner".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    let mut bytes = Vec::new();
    for row in
        rows.into_iter().filter(|row| row.stream() == culprit_core::telemetry::TelemetryStream::OperationalSamples)
    {
        serde_json::to_writer(
            &mut bytes,
            &culprit_core::telemetry::ExportedRecord::new(&source, &capture, row).unwrap(),
        )
        .unwrap();
        bytes.push(b'\n');
    }
    let guard =
        culprit_core::limits::OperationGuard::new(culprit_engine::incident_history::HISTORY_IMPORT_TIMEOUT).unwrap();
    let report = culprit_engine::incident_history::import_reader(bytes.as_slice(), &guard.handle()).unwrap();
    let result = value(&report, bytes.len() as u64).unwrap();
    let entry = &result["incident_history"]["entries"][0];
    let reasons = entry["reasons"].as_array().unwrap();
    assert!(
        reasons.contains(&serde_json::json!(culprit_core::t!("view.tempdb_question.internal", state = "unresolved")))
    );
    assert!(reasons.contains(&serde_json::json!(culprit_core::t!("view.tempdb_question.spill"))));
    assert_eq!(entry["original"], serde_json::Value::Null);
    assert!(entry.get("basis_context").is_none());
    assert_eq!(result["incident_history"]["provenance"], "imported-artifact");
}

#[test]
fn compact_history_keeps_canonical_compilation_context_and_all_competing_evidence_gaps() {
    let mut report = report();
    report.entries.truncate(1);
    report.entries[0].reasons = vec![
        culprit_core::t!("view.incident_history.compilation.independent"),
        culprit_core::t!("view.incident_history.compilation.gap.actual_plan"),
        culprit_core::t!("view.incident_history.compilation.gap.inputs"),
        culprit_core::t!("view.incident_history.compilation.gap.work"),
        culprit_core::t!("view.incident_history.compilation.gap.waits"),
        culprit_core::t!("view.incident_history.compilation.gap.context"),
    ];
    let projected = value(&report, 0).unwrap();
    assert_eq!(projected["incident_history"]["entries"][0]["reasons"], serde_json::json!(report.entries[0].reasons));
    assert_eq!(projected["incident_history"]["entries"][0]["analysis"], "unresolved");
}
