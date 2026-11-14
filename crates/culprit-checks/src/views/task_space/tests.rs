use super::*;
use culprit_core::telemetry::*;

fn observation() -> TaskSpaceObservation {
    TaskSpaceObservation {
        source: "native-task-space".into(),
        observed_at: "2026-11-13T10:00:00.2Z".into(),
        task: TaskSpaceIdentity { session_id: "42".into(), request_id: "0".into(), execution_context_id: "1".into() },
        native_database_id: "2".into(),
        bytes_per_page: 8192,
        user_allocated_pages: Some("18446744073709551615".into()),
        user_deallocated_pages: Some("0".into()),
        internal_allocated_pages: None,
        internal_deallocated_pages: Some("9007199254740993".into()),
        request: None,
        before_check: None,
        after_check: None,
        statement_identity: None,
        plan_identity: None,
    }
}
fn source(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: "native-task-space".into(), state, reason: None, reason_message: None }
}

#[test]
fn task_explanation_references_original_rows_without_net_footprint_timing_or_allocator_claims() {
    let row = observation();
    let report = explain(&[&row], Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.entries[0].task, row.task);
    assert_eq!(report.entries[0].observation_key, row.key().unwrap());
    assert_eq!(report.entries[0].activity, TaskAllocationActivity::Mixed);
    assert!(report.entries[0].explanation.contains("does not identify"));
    assert!(report.limitations.iter().any(|text| text.contains("not an atomic snapshot")));
    assert!(report.limitations.iter().any(|text| text.contains("no generation timestamp")));
    assert!(report.entries[0].verify.contains("do not subtract"));
    assert_eq!(row.user_allocated_pages.as_deref(), Some("18446744073709551615"));
    assert_eq!(row.user_deallocated_pages.as_deref(), Some("0"));
    assert_eq!(row.internal_allocated_pages, None);
}

fn measured_zero() -> TaskSpaceObservation {
    TaskSpaceObservation {
        user_allocated_pages: Some("0".into()),
        user_deallocated_pages: Some("0".into()),
        internal_allocated_pages: Some("0".into()),
        internal_deallocated_pages: Some("0".into()),
        ..observation()
    }
}

fn entry(row: &TaskSpaceObservation) -> TaskSpaceExplanation {
    explain(&[row], Some(&source(SourceState::Partial))).unwrap().entries.remove(0)
}

#[test]
fn user_internal_and_overlapping_activity_choose_different_investigations_without_summing_counters() {
    let mut row = measured_zero();
    row.user_allocated_pages = Some("9007199254740993".into());
    let original = row.clone();
    let user = entry(&row);
    assert_eq!(user.activity, TaskAllocationActivity::UserObjects);
    assert_eq!(user.investigation, t!("view.task_space.investigate.user"));

    row.user_allocated_pages = Some("0".into());
    row.internal_allocated_pages = Some("9007199254740993".into());
    let internal = entry(&row);
    assert_eq!(internal.activity, TaskAllocationActivity::InternalObjects);
    assert_eq!(internal.investigation, t!("view.task_space.investigate.internal"));
    assert!(internal.investigation.contains("index rebuild"));
    assert!(internal.explanation.contains("do not establish a spill"));

    row.user_allocated_pages = original.user_allocated_pages.clone();
    let mixed = entry(&row);
    assert_eq!(mixed.activity, TaskAllocationActivity::Mixed);
    assert_eq!(mixed.investigation, t!("view.task_space.investigate.mixed"));
    assert_eq!(mixed.observation_key, original.key().unwrap());
    assert_eq!(original.user_allocated_pages.as_deref(), Some("9007199254740993"));
    assert!(!serde_json::to_string(&mixed).unwrap().contains("18014398509481986"));
}

#[test]
fn deallocation_only_counts_remain_positive_activity_without_an_allocated_minus_deallocated_footprint() {
    let mut row = measured_zero();
    row.user_deallocated_pages = Some("18446744073709551615".into());
    assert_eq!(entry(&row).activity, TaskAllocationActivity::UserObjects);
    row.user_deallocated_pages = Some("0".into());
    row.internal_deallocated_pages = Some("9007199254740993".into());
    assert_eq!(entry(&row).activity, TaskAllocationActivity::InternalObjects);
    assert_eq!(row.internal_allocated_pages.as_deref(), Some("0"));
    assert_eq!(row.internal_deallocated_pages.as_deref(), Some("9007199254740993"));
}

#[test]
fn measured_zero_missing_and_positive_partial_counters_never_invent_a_healthy_or_exclusive_population() {
    let mut row = measured_zero();
    let zero = entry(&row);
    assert_eq!(zero.activity, TaskAllocationActivity::NoRecordedActivity);
    assert!(zero.explanation.starts_with(&t!("view.task_space.activity.zero")));
    row.internal_deallocated_pages = None;
    let partial = explain(&[&row], Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(partial.entries[0].activity, TaskAllocationActivity::Unknown);
    assert_eq!(partial.source_state, Some(SourceState::Partial));
    assert!(partial.limitations.contains(&t!("view.task_space.limit.incomplete_counters")));

    row.user_allocated_pages = Some("1".into());
    let positive = explain(&[&row], Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(positive.entries[0].activity, TaskAllocationActivity::UserObjects);
    assert!(positive.limitations.contains(&t!("view.task_space.limit.incomplete_counters")));
    row.user_allocated_pages = None;
    row.user_deallocated_pages = None;
    row.internal_allocated_pages = None;
    assert_eq!(entry(&row).activity, TaskAllocationActivity::Unknown);
}

fn checked_context(row: &mut TaskSpaceObservation) {
    let mut grant = crate::views::request_memory::tests::observation("context", ExecutionGrantState::Granted);
    crate::views::request_memory::tests::associated(&mut grant);
    let mut request = grant.request.unwrap();
    request.statement = DefinitionPayload::sql("SELECT unrelated_context FROM dbo.orders", culprit_core::Engine::Mssql);
    row.task.session_id = request.key.session.id.clone();
    row.request = Some(request);
    row.before_check =
        Some(TaskRequestCheck { observed_at: "2026-11-13T10:00:00.1Z".into(), state: TaskRequestState::SameStatement });
    row.after_check =
        Some(TaskRequestCheck { observed_at: "2026-11-13T10:00:00.3Z".into(), state: TaskRequestState::SameStatement });
    row.statement_identity = Some(NativeIdentity { id: "checked-statement".into(), qualifiers: Default::default() });
    assert!(row.is_valid());
}

#[test]
fn checked_or_changed_current_sql_never_selects_the_allocating_mechanism_or_a_maintenance_owner() {
    let mut row = measured_zero();
    row.internal_allocated_pages = Some("10".into());
    checked_context(&mut row);
    let checked = entry(&row);
    assert_eq!(checked.activity, TaskAllocationActivity::InternalObjects);
    assert!(checked.explanation.contains(&t!("view.task_space.context.checked")));
    assert!(!serde_json::to_string(&checked).unwrap().contains("unrelated_context"));

    row.request.as_mut().unwrap().statement =
        DefinitionPayload::sql("ALTER INDEX ix_orders ON dbo.orders REBUILD", culprit_core::Engine::Mssql);
    let maintenance = entry(&row);
    assert_eq!(maintenance.activity, checked.activity);
    assert_eq!(maintenance.investigation, checked.investigation);
    row.after_check.as_mut().unwrap().state = TaskRequestState::Changed;
    row.request.as_mut().unwrap().statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
    row.statement_identity = None;
    assert!(row.is_valid());
    let changed = entry(&row);
    assert_eq!(changed.activity, checked.activity);
    assert!(changed.explanation.contains(&t!("view.task_space.context.changed")));
    assert_eq!(changed.observation_key, checked.observation_key);
}

#[test]
fn reused_task_tuple_cannot_borrow_request_generation_or_become_two_independent_allocation_populations() {
    let mut first = measured_zero();
    first.user_allocated_pages = Some("5".into());
    checked_context(&mut first);
    let mut reused = first.clone();
    reused.request.as_mut().unwrap().key.session.generation = "new-login".into();
    reused.request.as_mut().unwrap().key.request.as_mut().unwrap().generation = "new-request".into();
    reused.user_allocated_pages = Some("1".into());
    assert!(reused.is_valid());
    assert_eq!(entry(&first).activity, entry(&reused).activity);
    assert_eq!(first.key().unwrap(), reused.key().unwrap());
    assert!(explain(&[&first, &reused], Some(&source(SourceState::Partial))).is_err());
}

#[test]
fn empty_unknown_and_unavailable_reads_remain_distinct_from_absent_tempdb_work() {
    let empty = explain(&[], Some(&source(SourceState::Empty))).unwrap();
    assert!(empty.summary.contains("bounded read"));
    let unavailable = explain(&[], Some(&source(SourceState::Unavailable))).unwrap();
    assert_eq!(unavailable.source_state, Some(SourceState::Unavailable));
    assert!(unavailable.limitations.iter().any(|text| text.contains("does not establish absent")));
    let unknown = explain(&[], None).unwrap();
    assert!(unknown.limitations.iter().any(|text| text.contains("coverage was not established")));
    assert!(explain(&[], Some(&source(SourceState::Collected))).is_err());
}

#[test]
fn duplicate_identity_conflicting_source_or_invalid_native_counter_is_rejected() {
    let row = observation();
    assert!(explain(&[&row, &row], Some(&source(SourceState::Partial))).is_err());
    assert!(explain(&[&row], Some(&source(SourceState::Unavailable))).is_err());
    let mut wrong = row.clone();
    wrong.source = "other-source".into();
    assert!(explain(&[&wrong], Some(&source(SourceState::Collected))).is_err());
    for invalid in ["-1", "1.5", "18446744073709551616", "NaN"] {
        let mut wrong = row.clone();
        wrong.internal_allocated_pages = Some(invalid.into());
        assert!(explain(&[&wrong], Some(&source(SourceState::Partial))).is_err());
    }
}
