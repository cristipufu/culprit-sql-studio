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
        ("temporary", Val::Int(0)),
        ("cache", Val::Int(0)),
        ("engine_edition", Val::Int(3)),
        ("database_edition", Val::Null),
        ("replica_id", Val::Null),
        ("updateability", Val::Text("READ_WRITE".into())),
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
    assert_eq!(counters[0].context, culprit_core::FileIoContext::STORAGE);
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
        assert!(
            sql.contains("IF @engine_edition = 5 AND (@database_edition = N'Hyperscale' OR @replica_id IS NOT NULL)")
        );
        let cache_call = sql.find("FROM sys.dm_io_virtual_file_stats(0, NULL)").unwrap();
        assert!(
            sql[..cache_call].contains("BEGIN"),
            "the special source requires positive native control-flow qualification"
        );
        assert_eq!(reader.calls.len(), 1, "metadata qualification shares the bounded counter statement");
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
    assert_eq!(app[0].scope, Some(FileScope::Database), "native scope does not depend on a visible database name");
    assert_eq!(app[0].name, "1");
}

#[tokio::test]
async fn shared_tempdb_is_explicit_and_zero_io_files_remain_in_raw_observations() {
    let temp = replace(
        &row(),
        &[("shared", Val::Int(1)), ("temporary", Val::Int(1)), ("database_name", Val::Text("tempdb".into()))],
    );
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

fn hyperscale(native_type: &str, updateability: Val) -> Rec {
    replace(
        &row(),
        &[
            ("engine_edition", Val::Int(5)),
            ("database_edition", Val::Text("Hyperscale".into())),
            ("native_type", Val::Text(native_type.into())),
            ("replica_id", Val::Text("91487244-730d-4b83-8e49-d482241c7288".into())),
            ("updateability", updateability),
        ],
    )
}

#[tokio::test]
async fn native_hyperscale_directions_and_replica_roles_survive_both_projection_paths() {
    use culprit_core::IoAccounting::*;
    for (native_type, role, expected) in [
        ("0", Val::Text("READ_WRITE".into()), (PageServerResponse, Simulated)),
        ("0", Val::Text("READ_ONLY".into()), (PageServerResponse, Simulated)),
        ("1", Val::Text("READ_WRITE".into()), (Unknown, LogServiceWrite)),
        ("1", Val::Text("READ_ONLY".into()), (ReplicaLogAccounting, ReplicaLogAccounting)),
        ("1", Val::Null, (Unknown, Unknown)),
        ("1", Val::Text("FUTURE_ROLE".into()), (Unknown, Unknown)),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![hyperscale(native_type, role)])]);
        let counters = read(&mut reader, Selection::Database, 50).await.unwrap();
        let context = &counters[0].context;
        assert_eq!((context.read, context.write), expected);
        assert_eq!(context.replica_id.as_deref(), Some("91487244-730d-4b83-8e49-d482241c7288"));
        assert_eq!(display(&counters)[0].context, *context);
        assert!(observations(counters).iter().all(OperationalObservation::is_valid));
    }
}

#[tokio::test]
async fn local_cache_is_shared_page_accounting_and_never_assigned_to_the_selected_database() {
    use culprit_core::IoAccounting;
    let cache = replace(
        &hyperscale("0", Val::Text("READ_WRITE".into())),
        &[
            ("cache", Val::Int(1)),
            ("database_id", Val::Int(0)),
            ("file_id", Val::Int(0)),
            ("native_type", Val::Null),
            ("database_name", Val::Null),
            ("file_name", Val::Null),
        ],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![cache])]);
    let counters = read(&mut reader, Selection::Database, 50).await.unwrap();
    assert_eq!(counters[0].kind, FileKind::Other);
    assert_eq!(counters[0].scope, FileScope::Server);
    assert_eq!(counters[0].database, None);
    assert_eq!(counters[0].context.read, IoAccounting::CachePage);
    assert_eq!(counters[0].context.write, IoAccounting::Unknown);
    assert_eq!(counters[0].context.replica_id, None, "a pool cache is not a selected-database replica counter");
    assert_eq!(display(&counters)[0].scope, Some(FileScope::Server));
    assert_eq!(observations(counters)[0].key, "0:0");
}

#[tokio::test]
async fn unknown_deployments_are_retained_without_defaulting_to_conventional_io() {
    for (engine, edition) in [
        (Val::Null, Val::Null),
        (Val::Int(5), Val::Null),
        (Val::Int(5), Val::Text("Future Tier".into())),
        (Val::Int(6), Val::Text("Premium".into())),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(
            &row(),
            &[("engine_edition", engine), ("database_edition", edition)],
        )])]);
        let counters = read(&mut reader, Selection::Database, 50).await.unwrap();
        assert_eq!(counters[0].context, culprit_core::FileIoContext::UNKNOWN);
        assert_eq!(counters[0].reads, "10", "unknown meaning does not erase measured counters");
    }
    for edition in ["Basic", "Standard", "Premium", "General Purpose", "Business Critical"] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(
            &row(),
            &[("engine_edition", Val::Int(5)), ("database_edition", Val::Text(edition.into()))],
        )])]);
        assert_eq!(
            read(&mut reader, Selection::Database, 50).await.unwrap()[0].context,
            culprit_core::FileIoContext::STORAGE
        );
    }
}

#[tokio::test]
async fn temporary_log_and_missing_hyperscale_metadata_do_not_inherit_commit_or_device_semantics() {
    use culprit_core::IoAccounting;
    let temp_log = replace(&row(), &[("native_type", Val::Text("1".into())), ("temporary", Val::Int(1))]);
    let mut reader = FakeRead::new(vec![Ok(vec![temp_log])]);
    assert_eq!(
        read(&mut reader, Selection::DatabaseAndSharedTempdb, 50).await.unwrap()[0].context.write,
        IoAccounting::NonDurableLog
    );
    let hidden = replace(&hyperscale("0", Val::Null), &[("native_type", Val::Null), ("replica_id", Val::Null)]);
    let mut reader = FakeRead::new(vec![Ok(vec![hidden])]);
    assert_eq!(
        read(&mut reader, Selection::Database, 50).await.unwrap()[0].context,
        culprit_core::FileIoContext::UNKNOWN
    );
}

#[tokio::test]
async fn inconsistent_native_context_or_unqualified_special_rows_are_source_failures() {
    for rows in [
        vec![row(), replace(&row(), &[("file_id", Val::Int(2)), ("engine_edition", Val::Int(5))])],
        vec![replace(&row(), &[("cache", Val::Int(1)), ("database_id", Val::Int(0))])],
        vec![replace(&hyperscale("0", Val::Null), &[("replica_id", Val::Text("not-a-native-guid".into()))])],
        vec![replace(
            &hyperscale("0", Val::Null),
            &[("replica_id", Val::Text("00000000-0000-0000-0000-000000000000".into()))],
        )],
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, Selection::Database, 50).await.is_err());
        assert_eq!(reader.calls.len(), 1, "invalid metadata cannot start a broadened fallback read");
    }
}
