use super::*;
use crate::resources::test_support::{FakeRead, replace};

pub(crate) fn layout(edition: i64, major: i64, view: i64, column: i64) -> Rec {
    Rec::of(&[
        ("edition", Val::Int(edition)),
        ("major", Val::Int(major)),
        ("native_view", Val::Int(view)),
        ("adr_column", Val::Int(column)),
    ])
}
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("pvs_filegroup_id", Val::Int(1)),
        ("persistent_version_store_size_kb", Val::Int(9_007_199_254_740_993)),
        ("online_index_version_store_size_kb", Val::Null),
        ("current_aborted_transaction_count", Val::Int(0)),
        ("oldest_active_transaction_id", Val::Int(9_007_199_254_740_993)),
        ("min_transaction_timestamp", Val::Int(-1)),
        ("offrow_version_cleaner_start_time", Val::Text("2020-01-01T07:58:30.1533333".into())),
        ("offrow_version_cleaner_end_time", Val::Null),
    ])
}
fn reader(row: Rec) -> FakeRead {
    FakeRead::new(vec![Ok(vec![layout(5, 12, 1, 1)]), Ok(vec![Rec::of(&[("adr", Val::Int(1))])]), Ok(vec![row])])
}
#[tokio::test]
async fn persistent_versions_keep_off_row_units_exact_counts_native_watermarks_and_local_cleaner_times() {
    let mut reader = reader(row());
    let sample = read(&mut reader).await.unwrap().unwrap();
    assert_eq!(sample.off_row_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(sample.online_index_kibibytes, None);
    assert_eq!(sample.aborted_transactions.as_deref(), Some("0"));
    assert_eq!(sample.oldest_active_transaction_id.as_deref(), Some("9007199254740993"));
    assert_eq!(sample.minimum_transaction_timestamp.as_deref(), Some("-1"));
    assert_eq!(sample.off_row_cleanup.started_without_offset.as_deref(), Some("2020-01-01T07:58:30.1533333"));
    assert_eq!(sample.off_row_cleanup.ended_without_offset, None);
    assert!(sample.available_columns.iter().any(|value| value == "online_index_version_store_size_kb"));
    assert!(!sample.available_columns.iter().any(|value| value == "oldest_aborted_transaction_id"));
    let app = signal(&sample).unwrap();
    assert_eq!(app.status, culprit_core::SignalStatus::Info);
    assert_eq!(app.measurement_scope, Some(culprit_core::MeasurementScope::Database));
    assert!(observation(sample).is_valid());
    assert_eq!(reader.calls.len(), 3);
}
#[tokio::test]
async fn known_unsupported_versions_and_disabled_adr_are_not_empty_stores_or_permission_failures() {
    let mut unsupported = FakeRead::new(vec![Ok(vec![layout(3, 14, 0, 0)])]);
    assert!(read(&mut unsupported).await.unwrap().is_none());
    assert_eq!(unsupported.calls.len(), 1);
    let mut disabled = FakeRead::new(vec![Ok(vec![layout(3, 16, 1, 1)]), Ok(vec![Rec::of(&[("adr", Val::Int(0))])])]);
    assert!(read(&mut disabled).await.unwrap().is_none());
    assert_eq!(disabled.calls.len(), 2);
    for metadata in [layout(5, 12, 0, 1), layout(3, 16, 1, 0)] {
        let mut unknown = FakeRead::new(vec![Ok(vec![metadata])]);
        assert!(read(&mut unknown).await.is_err());
        assert_eq!(unknown.calls.len(), 1, "hidden or unexpected metadata cannot become not-applicable");
    }
}
#[tokio::test]
async fn failures_unknown_sizes_or_malformed_values_never_turn_into_a_zero_or_healthy_grade() {
    for sample in [
        replace(&row(), &[("persistent_version_store_size_kb", Val::Null)]),
        replace(&row(), &[("persistent_version_store_size_kb", Val::Int(0))]),
    ] {
        let mut reader = reader(sample);
        let sample = read(&mut reader).await.unwrap().unwrap();
        let app = signal(&sample);
        assert_eq!(app.is_some(), sample.off_row_kibibytes.is_some());
        assert!(app.is_none_or(|signal| signal.status == culprit_core::SignalStatus::Info));
    }
    for invalid in [
        replace(&row(), &[("persistent_version_store_size_kb", Val::Int(-1))]),
        replace(&row(), &[("persistent_version_store_size_kb", Val::Float(1.0))]),
        replace(&row(), &[("offrow_version_cleaner_start_time", Val::Text("2020-01-01T07:00:00Z".into()))]),
    ] {
        assert!(read(&mut reader(invalid)).await.is_err());
    }
    let mut failed = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert!(read(&mut failed).await.is_err());
    assert_eq!(failed.calls.len(), 1);
    let mut empty =
        FakeRead::new(vec![Ok(vec![layout(5, 12, 1, 1)]), Ok(vec![Rec::of(&[("adr", Val::Int(1))])]), Ok(vec![])]);
    assert!(read(&mut empty).await.is_err(), "enabled ADR with no native observation is unknown, not zero");
}
