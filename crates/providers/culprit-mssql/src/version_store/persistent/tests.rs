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

#[tokio::test]
async fn documented_cleanup_skip_mechanisms_keep_distinct_native_counts_and_column_availability() {
    let original = row();
    let mut fields = original.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("pvs_off_row_page_skipped_oldest_active_xdesid", Val::Int(9_007_199_254_740_993)),
        ("pvs_off_row_page_skipped_min_useful_xts", Val::Int(0)),
        ("pvs_off_row_page_skipped_oldest_snapshot", Val::Int(7)),
        ("pvs_off_row_page_skipped_oldest_aborted_xdesid", Val::Null),
        ("pvs_off_row_page_skipped_transaction_not_cleaned", Val::Int(99)),
    ]);
    let native = Rec::of(&fields);
    let mut io = reader(native.clone());
    let sample = read(&mut io).await.unwrap().unwrap();
    assert_eq!(sample.pages_skipped_for_oldest_active_transaction.as_deref(), Some("9007199254740993"));
    assert_eq!(sample.pages_skipped_for_long_snapshot_scan.as_deref(), Some("0"));
    assert_eq!(sample.pages_skipped_for_online_index_rebuild.as_deref(), Some("7"));
    assert_eq!(sample.pages_skipped_for_oldest_aborted_transaction, None);
    assert_eq!(sample.pages_skipped_for_aborted_transactions.as_deref(), Some("99"));
    assert!(sample.available_columns.iter().any(|name| name == "pvs_off_row_page_skipped_oldest_aborted_xdesid"));
    assert_eq!(io.calls.len(), 3, "the existing SELECT * supplies all four fields without an extra query");

    let absent = Rec::of(
        &fields
            .into_iter()
            .filter(|(name, _)| *name != "pvs_off_row_page_skipped_oldest_aborted_xdesid")
            .collect::<Vec<_>>(),
    );
    let absent = read(&mut reader(absent)).await.unwrap().unwrap();
    assert_eq!(absent.pages_skipped_for_oldest_aborted_transaction, None);
    assert!(!absent.available_columns.iter().any(|name| name == "pvs_off_row_page_skipped_oldest_aborted_xdesid"));
    assert_ne!(absent, sample, "missing columns and native NULL are not interchangeable");

    for name in [
        "pvs_off_row_page_skipped_oldest_active_xdesid",
        "pvs_off_row_page_skipped_min_useful_xts",
        "pvs_off_row_page_skipped_oldest_snapshot",
        "pvs_off_row_page_skipped_oldest_aborted_xdesid",
    ] {
        for invalid in [Val::Int(-1), Val::Float(1.5)] {
            assert!(read(&mut reader(replace(&native, &[(name, invalid)]))).await.is_err());
        }
    }
}

#[tokio::test]
async fn adr_catalog_flag_uses_original_name_while_native_statistics_keep_the_current_id_namespace() {
    let mut native = reader(row());
    assert!(read(&mut native).await.unwrap().is_some());
    assert_eq!(native.calls.len(), 3);
    assert!(native.calls[1].sql.starts_with(crate::database_identity::catalog::BINDING));
    assert!(
        native.calls[1].sql.contains("FROM sys.databases WHERE name=@culprit_catalog_current_name OPTION(MAXDOP 1)")
    );
    assert!(!native.calls[1].sql.contains("database_id=DB_ID()"));
    assert_eq!(native.calls[2].sql, STATISTICS);
    assert!(native.calls[2].sql.contains("WHERE database_id=DB_ID()"));
}
