use super::*;
use crate::{
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("group_handle", Val::Int(7)),
        ("database_id", Val::Int(5)),
        ("object_id", Val::Int(100)),
        ("index_handle", Val::Int(8)),
        ("created_without_offset", Val::Text("2026-11-10T07:00:00.123".into())),
        ("last_seek", Val::Text("2026-11-10T07:01:00.123".into())),
        ("last_scan", Val::Null),
        ("schema_name", Val::Text("dbo".into())),
        ("table_name", Val::Text("Child".into())),
        ("equality_columns", Val::Text("[tenant]]id], [owner, name]".into())),
        ("inequality_columns", Val::Null),
        ("included_columns", Val::Text("[payload]".into())),
        ("user_seeks", Val::Int(2)),
        ("user_scans", Val::Int(0)),
        ("avg_total_user_cost", Val::Float(1.25)),
        ("avg_user_impact", Val::Float(85.0)),
        ("last_used", Val::Text("2026-11-10T07:00:00Z".into())),
    ])
}
fn link(group: i64, hash: &str) -> Rec {
    Rec::of(&[("group_handle", Val::Int(group)), ("query_hash", Val::Text(hash.into()))])
}
#[test]
fn native_identifier_lists_use_the_shared_lexer_and_reject_partial_or_ambiguous_input() {
    assert_eq!(
        bracket_list(Some("[tenant]]id], [owner, name], [a.b]".into())).unwrap(),
        vec!["tenant]id", "owner, name", "a.b"]
    );
    assert_eq!(bracket_list(None).unwrap(), Vec::<String>::new());
    for raw in ["", "[broken", "[a],", "[a] trailing", "[]", "[a] [b]", "'not an identifier'"] {
        assert!(bracket_list(Some(raw.into())).is_err(), "{raw}");
    }
}
#[tokio::test]
async fn no_recommendations_require_no_query_association_read() {
    let mut reader = FakeRead::new(vec![Ok(Vec::new())]);
    let read = application(&mut reader).await.unwrap();
    assert!(read.indexes.is_empty() && read.query_link_failure.is_none());
    assert_eq!(reader.calls.len(), 1);
}
#[tokio::test]
async fn native_links_are_database_scoped_deduplicated_and_attached_to_known_groups() {
    let hash = "0x0123456789ABCDEF";
    let mut reader =
        FakeRead::new(vec![Ok(vec![row()]), Ok(vec![link(7, hash), link(7, hash), link(8, "0x0000000000000001")])]);
    let read = application(&mut reader).await.unwrap();
    assert_eq!(read.indexes[0].equality, vec!["tenant]id", "owner, name"]);
    assert_eq!(read.indexes[0].query_hashes, vec![hash]);
    assert!(read.query_link_failure.is_none());
    assert!(reader.calls[1].sql.contains("WHERE mid.database_id=DB_ID()"));
    assert!(reader.calls[1].sql.contains("JOIN sys.dm_db_missing_index_groups"));
}
#[tokio::test]
async fn optional_association_failures_are_explicit_and_uncertain_native_work_fails_the_read() {
    let denied = Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture");
    let mut reader = FakeRead::new(vec![Ok(vec![row()]), Err(ReadError::Source(denied))]);
    let read = application(&mut reader).await.unwrap();
    assert_eq!(read.indexes.len(), 1);
    assert_eq!(read.query_link_failure.unwrap().code, culprit_core::ErrorCode::PermissionDenied);
    let mut reader =
        FakeRead::new(vec![Ok(vec![row()]), Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))]);
    assert_eq!(
        application(&mut reader).await.err().unwrap().detail,
        Some(ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed))
    );
    assert_eq!(reader.calls.len(), 2);
}
#[tokio::test]
async fn raw_advice_keeps_native_identity_exact_counters_and_unknown_associations() {
    let row = replace(&row(), &[("user_seeks", Val::Int(9_007_199_254_740_993))]);
    let mut reader = FakeRead::new(vec![Ok(vec![row.clone()]), Ok(Vec::new())]);
    let read = read(&mut reader, 10).await.unwrap();
    let sample = &read.indexes[0].sample;
    assert_eq!(sample.seeks, "9007199254740993");
    assert_eq!(sample.index_handle, 8);
    assert_eq!(sample.query_hashes, Some(Vec::new()));
    assert_eq!(sample.reset, culprit_core::ResetMarker::Unknown);
    assert!(sample.is_valid());
    let denied = Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture");
    let mut reader = FakeRead::new(vec![Ok(vec![row]), Err(ReadError::Source(denied))]);
    let read = super::read(&mut reader, 10).await.unwrap();
    assert!(read.indexes[0].sample.query_hashes.is_none());
    assert!(read.query_link_failure.is_some());
}

#[tokio::test]
async fn malformed_native_rows_stop_before_any_optional_query() {
    for change in [
        ("user_seeks", Val::Null),
        ("avg_total_user_cost", Val::Float(f64::NAN)),
        ("equality_columns", Val::Text("[broken".into())),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[change])])]);
        assert!(application(&mut reader).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}
