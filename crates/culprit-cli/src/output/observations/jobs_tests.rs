use super::*;
use culprit_collector::analytical::test_support::{MssqlAgentJobsCase, mssql_agent_jobs_evidence};
use culprit_core::telemetry::*;
#[test]
fn agent_brief_preserves_exact_native_tokens_scope_limits_and_safe_verification_from_one_pure_report() {
    let (source, records) = mssql_agent_jobs_evidence(MssqlAgentJobsCase::Failed);
    let rows = records
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect();
    let read = JobsSnapshot::from_source(&source, rows, true).unwrap();
    let report = culprit_checks::jobs::explain(&read).unwrap();
    let text = jobs(&report);
    assert!(text.contains("250101"));
    assert!(text.contains("2147483647"));
    assert!(text.contains(&report.context.observed_from));
    assert!(text.contains(&report.context.observed_until));
    for limitation in &report.limitations {
        assert!(text.contains(limitation));
    }
    assert!(text.contains(&report.entries[0].investigation));
    assert!(text.contains(&report.entries[0].verify));
    assert!(!text.contains("sp_start_job"));
    assert!(!text.contains("THROW"));
}

#[test]
fn scheduled_event_brief_uses_canonical_status_and_escapes_native_controls_without_sql_or_proof_payloads() {
    use culprit_collector::analytical::test_support::{MysqlEventsCase, mysql_events_evidence};
    let (source, records) = mysql_events_evidence(MysqlEventsCase::Disabled);
    let mut rows = records
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { unreachable!() };
            row
        })
        .collect::<Vec<_>>();
    let OperationalFact::MysqlScheduledEvent(event) = &mut rows[0].fact else { unreachable!() };
    event.name = "夜\\r\n\u{1b}[31m private-native-event".into();
    rows[0].key = event.key().unwrap();
    let read = JobsSnapshot::from_source(&source, rows, true).unwrap();
    let report = culprit_checks::jobs::explain(&read).unwrap();
    let text = jobs(&report);
    assert!(text.contains("夜"));
    assert!(text.contains("\\n"));
    assert!(text.contains("\\u{1b}"));
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains(&report.entries[0].investigation));
    assert!(text.contains(&report.entries[0].verify));
    assert!(text.contains(&culprit_core::t!("view.mysql_events.limit.outcome")));
    for forbidden in ["EVENT_DEFINITION", "DEFINER", "SHOW CREATE EVENT", "payload_sha256"] {
        assert!(!text.contains(forbidden));
    }
}
