use super::*;
use culprit_core::page_wait_report::PageTaskHolderBound;

fn fixture() -> (PageTaskHolderFocus, NativeReadContext, PageTaskHolderObservation) {
    let (writer, mut context, value) = super::writers::fixture();
    let mut page = *value.page.unwrap();
    let task = MssqlPageTaskWait {
        observed_at: page.observed_from.clone(),
        session_id: 61,
        request_id: 0,
        execution_context_id: 2,
        task_address: "0000000000000061".into(),
        wait_type: "PAGELATCH_EX".into(),
        wait_duration_ms: Some("9007199254740993".into()),
        resource: ObservedWaitResource {
            kind: WaitResourceKind::MemoryPage,
            native: NativeIdentity::new("7:1:4144").qualified("source", "sys.dm_os_waiting_tasks.resource_description"),
        },
        resource_address: Some("0000000000000044".into()),
        blocking_task_address: Some("0000000000000062".into()),
        blocking_session_id: Some(62),
        blocking_execution_context_id: Some(0),
    };
    page.task_wait = Some(task.clone());
    page.resource = task.resource.clone();
    let mut reference = writer.page;
    reference.native_key = NativeRecordIdentity::Operational { source: page.source.clone(), key: page.key().unwrap() };
    let focus = PageTaskHolderFocus {
        participant: writer.waiter.participant,
        page: reference,
        address: page.observed_address.unwrap(),
        task: task.clone(),
        target: page.target.clone().unwrap(),
        original_observed_until: page.observed_until.clone(),
    };
    context.observed_from = "2026-11-13T10:01:00Z".into();
    context.observed_until = "2026-11-13T10:01:01Z".into();
    page.observed_from = "2026-11-13T10:01:00.1Z".into();
    page.observed_until = "2026-11-13T10:01:00.9Z".into();
    page.before_check.as_mut().unwrap().observed_at = "2026-11-13T10:01:00.2Z".into();
    page.after_check.as_mut().unwrap().observed_at = "2026-11-13T10:01:00.8Z".into();
    page.task_wait.as_mut().unwrap().observed_at = page.observed_from.clone();
    let holder = MssqlPageTaskHolder {
        relation: PageTaskHolderRelation::QualifiedRequest,
        matching_owner_rows: 1,
        task: Some(MssqlPageTaskIdentity {
            task_address: "0000000000000062".into(),
            session_id: Some(62),
            request_id: Some(0),
            execution_context_id: 0,
        }),
        request: Some(Box::new(value.candidates[0].request.clone().unwrap())),
    };
    let before = MssqlPageTaskHolderPoint {
        observed_at: page.observed_from.clone(),
        database_writable: Some(true),
        request: PageWaitRequestState::SameStatement,
        wait: PageWaitRelation::OriginalObserved,
        holder,
    };
    let mut after = before.clone();
    after.observed_at = "2026-11-13T10:01:00.95Z".into();
    let mut after_task = task;
    after_task.observed_at = after.observed_at.clone();
    let row = PageTaskHolderObservation {
        observed_from: context.observed_from.clone(),
        observed_until: context.observed_until.clone(),
        focus_key: focus.key().unwrap(),
        target_state: PageWriterTargetState::CurrentMapped,
        final_target: Some(Box::new(PageTaskHolderTargetCheck {
            observed_at: "2026-11-13T10:01:00.92Z".into(),
            target: page.target.clone().unwrap(),
        })),
        page: Some(Box::new(page)),
        before,
        after_task: Some(after_task),
        after,
    };
    assert!(focus.accepts_observation(&context, &row));
    (focus, context, row)
}

#[test]
fn native_task_holder_is_an_exact_point_request_without_contribution_or_continuous_ownership() {
    let (focus, context, row) = fixture();
    let report = explain_task_holders(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.original_page, focus.page);
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.entries[0].bound, PageTaskHolderBound::Before);
    assert_eq!(report.entries[1].bound, PageTaskHolderBound::After);
    assert_eq!(report.entries[0].request.as_ref().unwrap().session.id, "62");
    assert_eq!(report.summary, t!("view.page_holders.summary.same"));
    for key in ["view.page_holders.limit.point", "view.page_holders.limit.contribution", "view.page_holders.limit.task"]
    {
        assert!(report.limitations.contains(&culprit_core::i18n::t(key, &[])));
    }
    assert_eq!(report.entries[0].investigation, t!("view.page_holders.investigate.request"));
    assert!(!serde_json::to_string(&report).unwrap().contains("private-writer-value"));
}

#[test]
fn an_owner_change_preserves_both_exact_generations_instead_of_summing_or_deduplicating_pids() {
    let (focus, context, mut row) = fixture();
    row.after.holder.request.as_mut().unwrap().key.request.as_mut().unwrap().generation = "native-request-2".into();
    let report = explain_task_holders(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.summary, t!("view.page_holders.summary.different"));
    assert_ne!(report.entries[0].request, report.entries[1].request);
    assert_eq!(
        report.entries[0].request.as_ref().unwrap().session,
        report.entries[1].request.as_ref().unwrap().session
    );
}

#[test]
fn sentinel_pointer_and_ambiguous_owners_remain_useful_unknowns_and_never_healthy_absence() {
    let (focus, context, mut row) = fixture();
    row.before.holder = MssqlPageTaskHolder {
        relation: PageTaskHolderRelation::Sentinel,
        matching_owner_rows: 0,
        task: None,
        request: None,
    };
    row.page.as_mut().unwrap().task_wait.as_mut().unwrap().blocking_session_id = Some(-4);
    row.after.holder = MssqlPageTaskHolder {
        relation: PageTaskHolderRelation::Ambiguous,
        matching_owner_rows: 2,
        task: None,
        request: None,
    };
    let report = explain_task_holders(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.summary, t!("view.page_holders.summary.unresolved"));
    assert_eq!(report.entries[0].explanation, t!("view.page_holders.relation.sentinel"));
    assert_eq!(report.entries[1].explanation, t!("view.page_holders.relation.ambiguous"));
    assert!(report.entries.iter().all(|entry| entry.request.is_none() && entry.statement.is_none()));
}

#[test]
fn missing_row_body_and_unavailable_sql_are_explicitly_distinct() {
    let (focus, context, mut row) = fixture();
    let missing = explain_task_holders(&focus, &context, None, Some(&source(SourceState::Collected))).unwrap();
    assert_eq!(missing.source_state, Some(SourceState::Partial));
    assert!(missing.entries.is_empty());
    assert_eq!(missing.summary, t!("view.page_holders.summary.unavailable"));
    row.before.holder.request.as_mut().unwrap().statement =
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    let unavailable = explain_task_holders(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(unavailable.entries[0].relation, PageTaskHolderRelation::QualifiedRequest);
    assert!(unavailable.entries[0].explanation.ends_with(&t!("view.page_holders.sql.unavailable")));
    assert!(!unavailable.entries[0].statement.as_ref().unwrap().is_available());
}

#[test]
fn foreign_source_scope_or_mutated_pointer_cannot_become_a_holder_report() {
    let (focus, mut context, mut row) = fixture();
    let mut wrong = source(SourceState::Partial);
    wrong.source = "another-source".into();
    assert!(explain_task_holders(&focus, &context, Some(&row), Some(&wrong)).is_err());
    row.after_task.as_mut().unwrap().blocking_session_id = Some(99);
    assert!(explain_task_holders(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).is_err());
    context.database_writable = Some(false);
    assert!(explain_task_holders(&focus, &context, None, Some(&source(SourceState::Partial))).is_err());
}

#[test]
fn a_positive_final_task_without_an_initial_mapping_cannot_be_reported_as_a_disappeared_wait() {
    let (focus, context, mut row) = fixture();
    row.page = None;
    row.final_target = None;
    row.before.wait = PageWaitRelation::NoLongerObserved;
    row.before.holder = MssqlPageTaskHolder {
        relation: PageTaskHolderRelation::Unavailable,
        matching_owner_rows: 0,
        task: None,
        request: None,
    };
    row.target_state = PageWriterTargetState::Unavailable;
    let report = explain_task_holders(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.summary, t!("view.page_holders.summary.mapping_unavailable"));
    assert_eq!(report.entries[1].relation, PageTaskHolderRelation::QualifiedRequest);
    row.target_state = PageWriterTargetState::NoLongerObserved;
    assert!(explain_task_holders(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).is_err());
}
