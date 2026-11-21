use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use test_support::*;
#[tokio::test]
async fn exact_native_integer_quantities_never_pass_through_a_float_and_all_files_keep_separate_meanings() {
    let group = replace(&space_row(), &[("total", Val::Int(9_007_199_254_740_993)), ("used", Val::Int(0))]);
    let readonly = replace(
        &file_row(),
        &[
            ("is_read_only", Val::Bool(true)),
            ("filegroup_read_only", Val::Bool(true)),
            ("file_state", Val::Int(6)),
            ("file_state_desc", Val::Text("OFFLINE".into())),
            ("used_pages", Val::Null),
        ],
    );
    let mut io = FakeRead::new(vec![Ok(vec![group, readonly])]);
    let rows = read(&mut io, Kind::Filegroups, 10).await.unwrap().observations(Kind::Filegroups).unwrap();
    let culprit_core::telemetry::OperationalFact::Space(space) = &rows[0].fact else { panic!() };
    assert_eq!(space.total_bytes.as_deref(), Some("9007199254740993"));
    assert_eq!(space.used_bytes.as_deref(), Some("0"));
    let culprit_core::telemetry::OperationalFact::MssqlDatabaseFile(file) = &rows[1].fact else { panic!() };
    assert_eq!(file.native_state, "6");
    assert_eq!(file.is_read_only, Some(true));
    assert!(file.used_pages.is_none());
    assert_eq!(io.calls.len(), 1);
    assert!(io.calls[0].sql.contains("LEFT JOIN sys.filegroups"));
    assert!(!io.calls[0].sql.contains("physical_name"));
    assert!(!io.calls[0].sql.contains("8192.0"));
    assert_ne!(rows[0].key, rows[1].key);
}
#[tokio::test]
async fn sparse_filestream_unknown_and_null_fields_do_not_become_allocated_disk_totals_or_used_zero() {
    for (snapshot, native_type, basis) in [
        (Some(false), 2, culprit_core::telemetry::StorageFileSizeBasis::ContainerUsed),
        (Some(true), 0, culprit_core::telemetry::StorageFileSizeBasis::SnapshotMaximum),
        (None, 255, culprit_core::telemetry::StorageFileSizeBasis::Unknown),
    ] {
        let row = replace(
            &file_row(),
            &[
                ("file_type", Val::Int(native_type)),
                ("database_is_snapshot", snapshot.map(Val::Bool).unwrap_or(Val::Null)),
                ("used_pages", Val::Null),
            ],
        );
        let mut io = FakeRead::new(vec![Ok(vec![row])]);
        let Facts::Files { files, .. } = read(&mut io, Kind::Filegroups, 10).await.unwrap() else { panic!() };
        assert_eq!(files[0].size_basis, basis);
        assert!(files[0].used_pages.is_none());
    }
}
#[tokio::test]
async fn malformed_negative_duplicate_empty_or_over_budget_results_fail_without_an_alias_or_retry() {
    for (rows, limit) in [
        (vec![], 10),
        (vec![space_row()], 10),
        (vec![file_row(), file_row()], 10),
        (vec![replace(&file_row(), &[("size_pages", Val::Int(-1))])], 10),
        (vec![space_row(), file_row()], 1),
        (vec![replace(&space_row(), &[("total", Val::Float(9_007_199_254_740_992.0))]), file_row()], 10),
    ] {
        let mut io = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut io, Kind::Filegroups, limit).await.is_err());
        assert_eq!(io.calls.len(), 1);
    }
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::ByteLimit,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let mut io = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        let error = read(&mut io, Kind::Filegroups, 10).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(io.calls.len(), 1);
    }
}

#[tokio::test]
async fn native_profile_and_canonical_admission_share_one_classification_and_reject_ineligible_usage() {
    for (native_type, native_state) in [(1, 0), (2, 0), (0, 6), (255, 0)] {
        let row = replace(&file_row(), &[("file_type", Val::Int(native_type)), ("file_state", Val::Int(native_state))]);
        let mut io = FakeRead::new(vec![Ok(vec![row])]);
        let error = read(&mut io, Kind::Filegroups, 10).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
        assert_eq!(io.calls.len(), 1);
    }
    for sparse in [None, Some(false), Some(true)] {
        let row = replace(
            &file_row(),
            &[
                ("database_is_snapshot", Val::Null),
                ("is_sparse", sparse.map(Val::Bool).unwrap_or(Val::Null)),
                ("used_pages", Val::Null),
            ],
        );
        let mut io = FakeRead::new(vec![Ok(vec![row])]);
        let Facts::Files { files, .. } = read(&mut io, Kind::Filegroups, 10).await.unwrap() else { panic!() };
        assert_eq!(files[0].kind, FileKind::Data);
        assert_eq!(files[0].size_basis, culprit_core::telemetry::StorageFileSizeBasis::Unknown);
        assert_eq!(files[0].is_sparse, sparse);
        assert!(files[0].database_is_snapshot.is_none() && files[0].is_valid());
        assert_eq!(io.calls.len(), 1);
    }
}

#[test]
fn complete_select_builders_preserve_all_existing_native_dispatch_bytes() {
    for (kind, original) in [
        (Kind::Log, log::query(Kind::Log)),
        (Kind::TempLog, log::query(Kind::TempLog)),
        (Kind::Filegroups, files_sql()),
        (Kind::Temp, TEMP_SQL.into()),
        (Kind::Sequences, SEQUENCES_SQL.into()),
    ] {
        assert_eq!(kind.sql(), format!("{original} OPTION(MAXDOP 1)"), "{kind:?}");
        assert_eq!(kind.sql().matches("OPTION(MAXDOP 1)").count(), 1, "{kind:?}: one complete serial SELECT");
    }
}

#[tokio::test]
async fn file_snapshot_basis_uses_original_catalog_name_without_filtering_or_adding_native_file_work() {
    let mut native = FakeRead::new(vec![Ok(vec![space_row(), file_row()])]);
    let files = read(&mut native, Kind::Filegroups, 10).await.unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(native.calls.len(), 1);
    let sql = &native.calls[0].sql;
    assert!(sql.starts_with(crate::database_identity::catalog::BINDING));
    assert!(sql.contains("FROM sys.databases WHERE name=@culprit_catalog_current_name) AS database_is_snapshot"));
    assert!(sql.contains("@culprit_catalog_current_name AS database_name,@culprit_catalog_current_id AS database_id"));
    assert!(sql.contains("FROM sys.database_files"));
    assert!(!sql.contains("WHERE database_id=DB_ID()"));
    assert_eq!(native.calls[0].limit, 11);
}
