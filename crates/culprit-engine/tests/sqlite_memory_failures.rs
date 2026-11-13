//! Native allocation codes survive the real application-log parser and existing pure diagnosis pipeline.
use culprit_core::{
    Actionability, Capability, Engine, Report, Severity, Snapshot, SourceOutcome, SourceState, source_id,
};
use serde_json::{Value, json};

fn analyze_log(code: Value) -> Report {
    let lines = [
        json!({
            "sql": "SELECT value FROM items WHERE id = ?",
            "ms": 12,
            "at": "2026-11-13T07:00:00Z",
        })
        .to_string(),
        json!({
            "sql": "SELECT value FROM items WHERE id = ?",
            "error_code": code,
            "at": "2026-11-13T07:59:00Z",
        })
        .to_string(),
    ];
    let (format, log) = culprit_querylog::read(lines.iter().map(String::as_str), Engine::Sqlite).expect("JSON log");
    assert!(!log.limited());
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../../fixtures/sqlite-demo.json")).expect("SQLite snapshot fixture");
    snapshot.taken_at = "2026-11-13T08:00:00Z".into();
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
    snapshot.operations.log_failures = log.failures();
    snapshot.workload = Some(log.workload(format, 100));
    culprit_checks::analyze(snapshot)
}

#[test]
fn symbolic_and_numeric_allocation_failures_keep_affected_statement_and_do_not_invent_an_untimed_execution() {
    for (native, expected) in [
        (json!("SQLITE_NOMEM"), "SQLITE_NOMEM"),
        (json!(7), "SQLITE_NOMEM"),
        (json!("7"), "SQLITE_NOMEM"),
        (json!("SQLITE_IOERR_NOMEM"), "SQLITE_IOERR_NOMEM"),
        (json!(3082), "SQLITE_IOERR_NOMEM"),
        (json!("3082"), "SQLITE_IOERR_NOMEM"),
    ] {
        let report = analyze_log(native);
        let workload = report.snapshot.workload.as_ref().expect("timed successful observation");
        assert_eq!(workload.total_executions, 1.0);
        assert_eq!(workload.total_time_ms, 12.0);
        let failures = &report.snapshot.operations.log_failures;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].code, expected);
        assert_eq!(failures[0].count, 1.0);
        let finding =
            report.findings.iter().find(|finding| finding.check == "logged-errors").expect("existing diagnosis");
        assert_eq!(finding.severity, Severity::Medium);
        assert_eq!(finding.actionability, Actionability::NeedsAttention);
        assert_eq!(finding.subject.name, failures[0].query_id);
        assert_eq!(finding.queries.len(), 1);
        assert_eq!(finding.queries[0].id, workload.queries[0].id);
        assert!(finding.summary.contains("does not identify the memory consumer"));
        assert!(finding.fix.as_ref().expect("bounded application investigation").sql.is_none());
    }
}

#[test]
fn unrecognized_memory_prose_remains_a_generic_failure_without_an_allocation_or_host_oom_diagnosis() {
    for description in ["out of memory", "killed by OOM killer", "memory exhausted", "SQLITE_NOMEM_UNKNOWN"] {
        let report = analyze_log(json!(description));
        assert_eq!(report.snapshot.operations.log_failures[0].code, "Logged SQL error");
        let finding =
            report.findings.iter().find(|finding| finding.check == "logged-errors").expect("retained unknown error");
        assert_eq!(finding.actionability, Actionability::Information);
        assert_eq!(finding.severity, Severity::Low);
        assert!(!finding.summary.contains("required to complete this operation"));
    }
}
