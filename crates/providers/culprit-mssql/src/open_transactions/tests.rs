use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::SourceState;

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("confirmed", Val::Int(1)),
        ("session_id", Val::Text("42".into())),
        ("session_generation", Val::Text("2020-01-01T07:00:00.1533333".into())),
        ("transaction_id", Val::Text("9007199254740993".into())),
        ("transaction_generation", Val::Text("2020-01-01T07:58:30.1533333".into())),
        ("native_observed_at", Val::Text("2020-01-01T08:00:00.1533333".into())),
        ("program_name", Val::Null),
        ("status", Val::Text("running".into())),
        ("log_record_count", Val::Int(0)),
        ("log_bytes_used", Val::Int(0)),
        ("begin_lsn", Val::Null),
        ("row_version_participant", Val::Int(1)),
        ("snapshot_isolation", Val::Int(1)),
        ("transaction_sequence_number", Val::Int(9_007_199_254_740_993)),
        ("versioning_age_seconds", Val::Int(30)),
    ])
}
#[tokio::test]
async fn oldest_native_holder_keeps_identity_ticks_precise_counters_and_two_different_ages() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let horizon = read(&mut reader, Selection::AllOtherUserSessions).await.unwrap();
    let raw = horizon.oldest_confirmed.as_ref().unwrap();
    assert_eq!(raw.transaction.id, "9007199254740993");
    assert_eq!(raw.session.generation, "2020-01-01T07:00:00.1533333");
    assert_eq!(raw.transaction.generation, "2020-01-01T07:58:30.1533333");
    assert_eq!(raw.age_seconds.as_deref(), Some("90"));
    assert_eq!(raw.versioning_age_seconds.as_deref(), Some("30"));
    assert_eq!(raw.transaction_sequence_number.as_deref(), Some("9007199254740993"));
    let app = application(&horizon, Some(1_048_576.0)).unwrap();
    assert_eq!(app.seconds, 90.0);
    assert_eq!(app.held_bytes, Some(1_048_576.0));
    assert_eq!(app.wrote, Some(false));
    assert!(observation(horizon).is_valid());
}
#[tokio::test]
async fn row_version_writers_are_distinct_from_snapshot_isolation_and_own_sessions_remain_in_raw() {
    let writer = replace(
        &row(),
        &[
            ("snapshot_isolation", Val::Int(0)),
            ("log_record_count", Val::Int(2)),
            ("begin_lsn", Val::Text("1234567890123456789012345".into())),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![writer.clone()]), Ok(vec![writer])]);
    let raw = read(&mut reader, Selection::AllOtherUserSessions).await.unwrap();
    read(&mut reader, Selection::Application).await.unwrap();
    let raw = raw.oldest_confirmed.unwrap();
    assert!(raw.row_version_participant);
    assert_eq!(raw.snapshot_isolation, Some(false));
    assert_eq!(raw.begin_lsn.as_deref(), Some("1234567890123456789012345"));
    assert_eq!(reader.calls[0].sql, reader.calls[1].sql, "the native grammar stays static");
    assert!(matches!(reader.calls[0].parameters[0], Parameter::Integer(0)));
    assert!(matches!(reader.calls[1].parameters[0], Parameter::Integer(1)));
}
#[tokio::test]
async fn a_successful_empty_horizon_is_a_measurement_and_a_failure_is_unavailable() {
    let mut reader = FakeRead::new(vec![Ok(vec![]), Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    let empty = read(&mut reader, Selection::Application).await.unwrap();
    assert!(empty.oldest_confirmed.is_none());
    assert!(observation(empty.clone()).is_valid());
    let mut coverage = Coverage::default();
    assert!(record_application_result(&mut coverage, Ok(empty), None).is_none());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Empty);
    let mut coverage = Coverage::default();
    assert!(record_application_result(&mut coverage, read(&mut reader, Selection::Application).await, None).is_none());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Unavailable);
}
#[tokio::test]
async fn future_native_begin_time_does_not_turn_a_clock_jump_into_a_healthy_zero_age() {
    let future = replace(&row(), &[("transaction_generation", Val::Text("2020-01-01T08:00:00.1533334".into()))]);
    let mut reader = FakeRead::new(vec![Ok(vec![future])]);
    let raw = read(&mut reader, Selection::Application).await.unwrap();
    assert_eq!(raw.oldest_confirmed.as_ref().unwrap().age_seconds, None);
    let mut coverage = Coverage::default();
    assert!(record_application_result(&mut coverage, Ok(raw), None).is_none());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
}
#[tokio::test]
async fn malformed_or_ineligible_native_rows_cannot_become_cleanup_evidence() {
    for rows in [
        vec![row(), row()],
        vec![replace(&row(), &[("transaction_generation", Val::Text("2020-01-01T07:58:30.15333331".into()))])],
        vec![replace(&row(), &[("log_record_count", Val::Int(-1))])],
        vec![replace(&row(), &[("row_version_participant", Val::Int(2))])],
        vec![replace(
            &row(),
            &[
                ("row_version_participant", Val::Int(0)),
                ("snapshot_isolation", Val::Null),
                ("transaction_sequence_number", Val::Null),
                ("versioning_age_seconds", Val::Null),
            ],
        )],
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, Selection::Application).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}
#[tokio::test]
async fn logged_idle_work_keeps_the_app_projection_without_attributing_database_versions_to_it() {
    let writer = replace(
        &row(),
        &[
            ("status", Val::Text("sleeping".into())),
            ("row_version_participant", Val::Int(0)),
            ("snapshot_isolation", Val::Null),
            ("transaction_sequence_number", Val::Null),
            ("versioning_age_seconds", Val::Null),
            ("log_record_count", Val::Int(3)),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![writer])]);
    let raw = read(&mut reader, Selection::Application).await.unwrap();
    let app = application(&raw, Some(1_048_576.0)).unwrap();
    assert_eq!(app.activity, TransactionActivity::IdleInTransaction);
    assert_eq!(app.wrote, Some(true));
    assert_eq!(app.held_bytes, None);
}

#[tokio::test]
async fn global_versioning_membership_without_database_cleanup_evidence_is_retained_but_not_graded() {
    let ambiguous = replace(&row(), &[("confirmed", Val::Int(0)), ("snapshot_isolation", Val::Int(0))]);
    let mut reader = FakeRead::new(vec![Ok(vec![ambiguous])]);
    let horizon = read(&mut reader, Selection::Application).await.unwrap();
    assert!(horizon.oldest_confirmed.is_none());
    assert!(horizon.oldest_unattributed_versioning.is_some());
    let mut coverage = Coverage::default();
    assert!(record_application_result(&mut coverage, Ok(horizon), None).is_none());
    assert_eq!(coverage.outcomes()[0].state, SourceState::Partial);
}
