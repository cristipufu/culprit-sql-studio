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
    assert!(report.entries[0].explanation.contains("does not identify"));
    assert!(report.limitations.iter().any(|text| text.contains("not an atomic snapshot")));
    assert!(report.limitations.iter().any(|text| text.contains("no generation timestamp")));
    assert!(report.entries[0].verify.contains("do not subtract"));
    assert_eq!(row.user_allocated_pages.as_deref(), Some("18446744073709551615"));
    assert_eq!(row.user_deallocated_pages.as_deref(), Some("0"));
    assert_eq!(row.internal_allocated_pages, None);
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
    let mut wrong = row;
    wrong.internal_allocated_pages = Some("-1".into());
    assert!(explain(&[&wrong], Some(&source(SourceState::Collected))).is_err());
}
