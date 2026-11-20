use super::*;
use culprit_collector::analytical::test_support::task_space_evidence_cases;
use culprit_core::{TaskAllocationActivity, telemetry::*};

fn observation() -> TaskSpaceObservation {
    let (_, records) = task_space_evidence_cases().remove(0);
    let TelemetryRecord::Operational(row) = records.into_iter().next().unwrap() else { unreachable!() };
    let OperationalFact::TaskSpace(mut task) = row.fact else { unreachable!() };
    task.user_allocated_pages = Some("0".into());
    task.user_deallocated_pages = Some("0".into());
    task.internal_allocated_pages = Some("0".into());
    task.internal_deallocated_pages = Some("0".into());
    *task
}

fn report(rows: &[TaskSpaceObservation], state: SourceState) -> TaskSpaceReport {
    let source =
        SourceOutcome { source: culprit_core::source_id::TASK_SPACE.into(), state, reason: None, reason_message: None };
    culprit_checks::task_space::explain(&rows.iter().collect::<Vec<_>>(), Some(&source)).unwrap()
}

fn assert_no_native_payload(output: &str) {
    for native in ["native-task-sql-handle", "native-task-plan-handle", "9007199254740993", "9264"] {
        assert!(!output.contains(native), "{native}");
    }
}

#[test]
fn user_internal_and_mixed_allocation_classes_keep_the_shared_explanation_and_context() {
    for (user, internal, activity) in [
        (true, false, TaskAllocationActivity::UserObjects),
        (false, true, TaskAllocationActivity::InternalObjects),
        (true, true, TaskAllocationActivity::Mixed),
    ] {
        let mut row = observation();
        if user {
            row.user_allocated_pages = Some("9007199254740993".into());
        }
        if internal {
            row.internal_allocated_pages = Some("9007199254740993".into());
        }
        let report = report(&[row], SourceState::Collected);
        assert_eq!(report.entries[0].activity, activity);
        let output = task_space(&report);
        assert!(output.contains(&report.entries[0].explanation));
        assert!(output.contains(&t!("view.task_space.entry.summary")));
        assert!(output.contains(&t!("view.task_space.context.checked")));
        assert!(output.contains(&report.limitations[0]));
        assert_no_native_payload(&output);
    }
}

#[test]
fn changed_or_missing_current_request_context_does_not_displace_recorded_allocation_activity() {
    for (state, context_key) in [
        (TaskRequestState::Changed, "view.task_space.context.changed"),
        (TaskRequestState::Missing, "view.task_space.context.missing"),
    ] {
        let mut row = observation();
        row.user_allocated_pages = Some("9007199254740993".into());
        row.after_check.as_mut().unwrap().state = state;
        row.request.as_mut().unwrap().statement =
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
        row.statement_identity = None;
        row.plan_identity = None;
        let report = report(&[row], SourceState::Partial);
        assert_eq!(report.entries[0].activity, TaskAllocationActivity::UserObjects);
        let output = task_space(&report);
        assert!(output.contains(&culprit_core::i18n::t(context_key, &[])));
        assert!(output.contains(&t!("view.task_space.activity.user")));
        assert!(output.contains(&t!("view.task_space.entry.summary")));
        assert!(!output.contains(&t!("view.task_space.context.checked")));
        assert!(output.contains(&t!("evidence.cli.source_partial")));
        assert_no_native_payload(&output);
    }
}

#[test]
fn a_measured_zero_unknown_counters_and_an_unavailable_source_remain_distinct() {
    let zero = observation();
    let zero_report = report(std::slice::from_ref(&zero), SourceState::Collected);
    assert_eq!(zero_report.entries[0].activity, TaskAllocationActivity::NoRecordedActivity);
    let measured = task_space(&zero_report);
    assert!(measured.contains(&t!("view.task_space.activity.zero")));
    assert!(!measured.contains(&t!("view.task_space.activity.unknown")));

    let mut unknown = zero;
    unknown.user_allocated_pages = None;
    let unknown_report = report(&[unknown], SourceState::Partial);
    assert_eq!(unknown_report.entries[0].activity, TaskAllocationActivity::Unknown);
    let unknown = task_space(&unknown_report);
    assert!(unknown.contains(&t!("view.task_space.activity.unknown")));
    assert!(!unknown.contains(&t!("view.task_space.activity.zero")));
    assert!(unknown.contains(&t!("evidence.cli.source_partial")));

    let unavailable = task_space(&report(&[], SourceState::Unavailable));
    assert!(unavailable.contains(&t!("evidence.cli.source_unavailable")));
    assert!(!unavailable.contains(&t!("view.task_space.activity.zero")));
    assert!(!unavailable.contains(&t!("view.task_space.activity.unknown")));
    assert!(!unavailable.contains('•'));
}

#[test]
fn task_details_and_entry_counts_keep_the_common_cli_output_limits_without_exporting_native_rows() {
    let rows = (0..8)
        .map(|index| {
            let mut row = observation();
            row.task.execution_context_id = index.to_string();
            row.user_allocated_pages = Some("9007199254740993".into());
            row
        })
        .collect::<Vec<_>>();
    let mut report = report(&rows, SourceState::Collected);
    for (index, entry) in report.entries.iter_mut().enumerate() {
        entry.title = format!("task-entry-{index}");
        entry.explanation = format!("{}hidden-detail-tail", "列".repeat(OBSERVATION_DETAIL_CHARS * 2));
    }
    let output = task_space(&report);
    let entries = output.lines().filter_map(|line| line.strip_prefix("• ")).collect::<Vec<_>>();
    assert_eq!(entries.len(), super::super::BRIEF_FINDING_LIMIT);
    assert!(entries.iter().all(|line| line.chars().count() <= OBSERVATION_DETAIL_CHARS + 32));
    assert!(entries.iter().all(|line| line.ends_with('…')));
    assert_eq!(
        output.chars().filter(|character| *character == '列').count(),
        super::super::BRIEF_FINDING_LIMIT * OBSERVATION_DETAIL_CHARS
    );
    assert!(output.contains(&t!("view.task_space.current_request.qualification")));
    assert!(output.contains(&report.entries[0].verify));
    assert!(!output.contains("task-entry-3") && !output.contains("hidden-detail-tail"));
    for row in &rows {
        assert!(!output.contains(&row.key().unwrap()));
    }
    assert_no_native_payload(&output);
}

#[test]
fn qualified_current_statement_and_its_own_interval_are_visible_without_becoming_an_allocator() {
    let row = observation();
    let expected = row.clone();
    let report = report(&[row], SourceState::Partial);
    let context = report.entries[0].current_request.as_ref().unwrap();
    let output = task_space(&report);
    assert!(output.contains("SELECT note FROM dbo.orders"));
    assert!(output.contains(&t!("view.task_space.current_request.title")));
    assert!(output.contains(&t!("view.task_space.current_request.qualification")));
    assert!(output.contains(&context.observed_from) && output.contains(&context.observed_until));
    assert_ne!(context.observed_from, expected.observed_at);
    assert_eq!(context.request, expected.request.unwrap().key);
    assert_no_native_payload(&output);
}

#[test]
fn unavailable_sql_reasons_and_withheld_context_have_distinct_cli_output() {
    for reason in [
        PayloadUnavailable::Restricted,
        PayloadUnavailable::Encrypted,
        PayloadUnavailable::Missing,
        PayloadUnavailable::Oversized,
        PayloadUnavailable::RedactionFailed,
        PayloadUnavailable::NotCollected,
        PayloadUnavailable::SourceUnavailable,
    ] {
        let mut row = observation();
        row.request.as_mut().unwrap().statement = DefinitionPayload::Unavailable { reason };
        row.statement_identity = None;
        row.plan_identity = None;
        let report = report(&[row], SourceState::Partial);
        assert!(report.entries[0].current_request.is_some());
        let output = task_space(&report);
        assert!(output.contains(&statement::sql(Some(&DefinitionPayload::Unavailable { reason }))));
        assert!(!output.contains("SELECT note"));
    }
    let row = observation();
    let mut report = report(&[row], SourceState::Partial);
    report.entries[0].current_request = None;
    let output = task_space(&report);
    assert!(!output.contains("SELECT note") && !output.contains(&t!("view.task_space.current_request.title")));
}

#[test]
fn long_unicode_statement_and_client_context_are_bounded_without_hiding_verification() {
    let mut row = observation();
    let raw = format!("SELECT {} FROM dbo.orders WHERE id=9264", "長".repeat(1000));
    row.request.as_mut().unwrap().statement = DefinitionPayload::sql(&raw, culprit_core::Engine::Mssql);
    row.request.as_mut().unwrap().client =
        Some(ActivityClient { application: Some("界".repeat(100)), user: None, host: None });
    let report = report(&[row], SourceState::Collected);
    let output = task_space(&report);
    assert!(output.contains('…'));
    assert!(!output.contains(&raw) && !output.contains("9264"));
    assert!(output.chars().filter(|character| *character == '長').count() <= 100);
    assert!(output.chars().filter(|character| *character == '界').count() <= 100);
    assert!(output.contains(&report.entries[0].investigation));
    assert!(output.contains(&report.entries[0].verify));
    assert_no_native_payload(&output);
}
