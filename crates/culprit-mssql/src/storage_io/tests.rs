use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(2)),
        ("file_id", Val::Int(1)),
        ("database_name", Val::Text("shop".into())),
        ("file_name", Val::Text("shop_data".into())),
        ("native_type", Val::Text("0".into())),
        ("shared", Val::Int(0)),
        ("reads", Val::Int(10)),
        ("read_ms", Val::Int(40)),
        ("read_bytes", Val::Int(9_007_199_254_740_993)),
        ("writes", Val::Int(900)),
        ("write_ms", Val::Int(1800)),
        ("write_bytes", Val::Int(900)),
        ("size_on_disk_bytes", Val::Int(8192)),
        ("sample_ms", Val::Int(1000)),
    ])
}

#[tokio::test]
async fn native_file_identity_and_integer_costs_survive_export_and_application_projection() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let counters = read(&mut reader, Selection::Database, 500).await.unwrap();
    assert_eq!(counters[0].kind, FileKind::Data, "database id 2 does not identify tempdb in Azure SQL");
    assert_eq!(counters[0].read_bytes.as_deref(), Some("9007199254740993"));
    assert_eq!(counters[0].reset, ResetMarker::Unknown);
    let app = display(&counters);
    assert_eq!((app[0].scope, app[0].database.as_deref()), (Some(FileScope::Database), Some("shop")));
    assert_eq!((app[0].reads, app[0].write_ms), (10.0, Some(1800.0)));
    assert_eq!(app[0].stats_reset, None);
    let raw = observations(counters);
    assert_eq!(raw[0].key, "2:1");
    assert!(raw[0].is_valid());
}

#[tokio::test]
async fn database_and_tempdb_reads_never_enumerate_other_databases_or_retry_a_failed_read() {
    for selection in [Selection::Database, Selection::DatabaseAndSharedTempdb] {
        let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
        read(&mut reader, selection, 100).await.unwrap();
        let sql = &reader.calls[0].sql;
        assert!(sql.contains("sys.dm_io_virtual_file_stats(DB_ID(), NULL)"));
        assert!(!sql.contains("sys.dm_io_virtual_file_stats(NULL"));
        assert!(sql.contains("LEFT JOIN sys.database_files"));
        assert_eq!(
            sql.contains("sys.dm_io_virtual_file_stats(2, NULL)"),
            selection == Selection::DatabaseAndSharedTempdb
        );
        assert!(sql.contains("WHERE vfs.database_id = DB_ID()"));
    }
    let mut reader = FakeRead::new(vec![Err(crate::read::ReadError::Stopped(CollectionStop::TimedOut))]);
    assert!(read(&mut reader, Selection::DatabaseAndSharedTempdb, 100).await.is_err());
    assert_eq!(reader.calls.len(), 1);
}

#[tokio::test]
async fn missing_catalog_metadata_preserves_the_native_counter_identity_without_guessing_a_file_kind() {
    let missing =
        replace(&row(), &[("file_name", Val::Null), ("native_type", Val::Null), ("database_name", Val::Null)]);
    let mut reader = FakeRead::new(vec![Ok(vec![missing])]);
    let counters = read(&mut reader, Selection::Database, 500).await.unwrap();
    assert_eq!(counters[0].kind, FileKind::Other);
    assert_eq!(counters[0].native_name, None);
    let app = display(&counters);
    assert_eq!(app[0].scope, None);
    assert_eq!(app[0].name, "1");
}

#[tokio::test]
async fn shared_tempdb_is_explicit_and_zero_io_files_remain_in_raw_observations() {
    let temp = replace(&row(), &[("shared", Val::Int(1)), ("database_name", Val::Text("tempdb".into()))]);
    let zero = replace(&row(), &[("file_id", Val::Int(2)), ("reads", Val::Int(0)), ("writes", Val::Int(0))]);
    let mut reader = FakeRead::new(vec![Ok(vec![temp, zero])]);
    let counters = read(&mut reader, Selection::DatabaseAndSharedTempdb, 500).await.unwrap();
    assert_eq!(counters.len(), 2);
    assert_eq!(counters[0].kind, FileKind::Temp);
    assert_eq!(counters[0].scope, FileScope::Server);
    assert_eq!(display(&counters).len(), 1);
    assert_eq!(observations(counters).len(), 2);
}

#[tokio::test]
async fn overflow_duplicate_native_ids_and_invalid_counters_are_explicit_source_failures() {
    for (limit, rows) in [
        (1, vec![row(), replace(&row(), &[("file_id", Val::Int(2))])]),
        (500, vec![row(), row()]),
        (500, vec![replace(&row(), &[("read_ms", Val::Null)])]),
        (500, vec![replace(&row(), &[("reads", Val::Int(-1))])]),
        (500, vec![replace(&row(), &[("database_id", Val::Int(0))])]),
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, Selection::Database, limit).await.is_err());
    }
}
