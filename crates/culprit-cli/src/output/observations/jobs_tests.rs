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
