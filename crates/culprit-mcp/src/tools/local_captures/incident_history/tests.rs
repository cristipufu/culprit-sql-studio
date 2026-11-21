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
