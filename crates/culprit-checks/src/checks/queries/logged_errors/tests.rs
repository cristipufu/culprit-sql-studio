use super::*;
use culprit_core::{Capability, SourceOutcome, SourceState, source_id};

fn snapshot(code: &str, last_at: Option<&str>) -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../../../../../fixtures/sqlite-demo.json")).expect("SQLite fixture");
    snapshot.taken_at = "2026-10-20T12:00:00Z".into();
    snapshot.options = None;
    if !snapshot.server.capabilities.contains(&Capability::QueryLog) {
        snapshot.server.capabilities.push(Capability::QueryLog);
    }
    snapshot.sources.retain(|source| source.source != source_id::QUERY_LOG);
    snapshot.sources.push(SourceOutcome {
        source: source_id::QUERY_LOG.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    snapshot.operations.log_failures = vec![LoggedFailure {
        query_id: "affected-statement".into(),
        statement: "SELECT value FROM items WHERE id = ?".into(),
        code: code.into(),
        count: 1.0,
        first_at: last_at.map(str::to_owned),
        last_at: last_at.map(str::to_owned),
    }];
    snapshot
}

#[test]
fn native_allocation_failure_codes_identify_an_affected_operation_without_naming_a_memory_owner() {
    for code in ["SQLITE_NOMEM", "SQLITE_IOERR_NOMEM"] {
        let snapshot = snapshot(code, Some("2026-10-20T11:59:00Z"));
        let mut cases = logged_errors(&snapshot);
        assert_eq!(cases.len(), 1);
        let case = cases.pop().expect("one allocation failure");
        assert!(case.code == ErrorCode::Allocation);
        assert!(case.critical);
        let finding = logged_error_finding(&snapshot, case);
        assert_eq!(finding.actionability, Actionability::NeedsAttention);
        assert_eq!(finding.severity, Severity::Medium);
        assert_eq!(finding.subject.name, "affected-statement");
        assert!(finding.summary.contains("required to complete this operation"));
        assert!(finding.summary.contains("does not identify the memory consumer"));
        assert!(finding.fix.expect("bounded next investigation").sql.is_none());
        let report = crate::analyze(snapshot);
        let finding =
            report.findings.iter().find(|finding| finding.check == "logged-errors").expect("pipeline diagnosis");
        assert_eq!(finding.actionability, Actionability::NeedsAttention);
        assert_eq!(finding.subject.name, "affected-statement");
    }
}

#[test]
fn an_old_undated_or_future_failure_is_retained_without_a_current_incident_claim() {
    for at in [None, Some("2026-10-01T08:00:00Z"), Some("2026-10-21T12:00:00Z")] {
        let snapshot = snapshot("SQLITE_NOMEM", at);
        let case = logged_errors(&snapshot).pop().expect("native positive evidence is retained");
        assert!(case.code == ErrorCode::Allocation);
        assert!(!case.critical);
        let finding = logged_error_finding(&snapshot, case);
        assert_eq!(finding.actionability, Actionability::Information);
        assert_eq!(finding.severity, Severity::Low);
    }
}

#[test]
fn other_codes_and_missing_or_invalid_counts_do_not_establish_allocation_failures() {
    for code in ["SQLITE_FULL", "SQLITE_INTERRUPT", "SIGKILL", "SQLITE_UNKNOWN"] {
        let snapshot = snapshot(code, Some("2026-10-20T11:59:00Z"));
        assert!(logged_errors(&snapshot).iter().all(|case| case.code != ErrorCode::Allocation));
    }
    for count in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut snapshot = snapshot("SQLITE_NOMEM", Some("2026-10-20T11:59:00Z"));
        snapshot.operations.log_failures[0].count = count;
        assert!(logged_errors(&snapshot).is_empty());
    }
    let mut snapshot = snapshot("SQLITE_NOMEM", None);
    snapshot.operations.log_failures.clear();
    assert!(logged_errors(&snapshot).is_empty());
}
