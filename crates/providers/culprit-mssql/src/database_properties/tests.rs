use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::ErrorDetail;

pub(crate) fn row(at: &str) -> Rec {
    Rec::of(&[
        ("observed_at", Val::Text(at.into())),
        ("metadata_visibility", Val::Int(1)),
        ("property_error_number", Val::Int(0)),
        ("selected_database_id", Val::Int(5)),
        ("selected_database", Val::Text("sample".into())),
        ("database_id", Val::Int(5)),
        ("database_name", Val::Text("sample".into())),
        ("state", Val::Int(0)),
        ("state_desc", Val::Text("ONLINE".into())),
        ("user_access", Val::Int(0)),
        ("user_access_desc", Val::Text("MULTI_USER".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("is_read_only", Val::Bool(false)),
        ("collation_name", Val::Text("Latin1_General_100_CI_AS".into())),
        ("compatibility_level", Val::Int(160)),
        ("snapshot_isolation_state", Val::Int(1)),
        ("snapshot_isolation_state_desc", Val::Text("ON".into())),
        ("is_read_committed_snapshot_on", Val::Bool(true)),
        ("is_auto_create_stats_on", Val::Bool(true)),
        ("is_auto_update_stats_on", Val::Bool(true)),
        ("is_auto_update_stats_async_on", Val::Bool(false)),
        ("is_parameterization_forced", Val::Bool(false)),
        ("is_query_store_on", Val::Bool(true)),
        ("page_verify_option", Val::Int(2)),
        ("page_verify_option_desc", Val::Text("CHECKSUM".into())),
        ("recovery_model", Val::Int(1)),
        ("recovery_model_desc", Val::Text("FULL".into())),
        ("target_recovery_time_in_seconds", Val::Int(0)),
        ("last_good_checkdb_without_offset", Val::Text("2019-12-28T23:10:00.003".into())),
    ])
}
fn at() -> &'static str {
    "2026-11-18T12:00:00.0000001Z"
}

#[tokio::test]
async fn the_one_selected_catalog_read_keeps_native_codes_zeroes_and_connection_updateability_separate() {
    let native = replace(&row(at()), &[("updateability", Val::Text("READ_ONLY".into()))]);
    let mut io = FakeRead::new(vec![Ok(vec![native])]);
    let read = read(&mut io).await.unwrap();
    let point = read.properties.unwrap();
    assert_eq!(point.catalog_read_only, Some(false));
    assert_eq!(point.writable(), Some(false));
    assert_eq!(point.catalog_state.code, Some(0));
    assert_eq!(point.target_recovery_time_seconds, Some(0));
    assert_eq!(point.observed_at, at());
    assert_eq!(point.database, read.database);
    assert_eq!(io.calls.len(), 1);
    assert_eq!(io.calls[0].limit, 2, "second native row is an ambiguity witness");
    assert!(io.calls[0].parameters.is_empty());
    assert!(io.calls[0].sql.contains("d.database_id=DB_ID()"));
}

#[tokio::test]
async fn null_fields_and_unrecognized_codes_are_not_replaced_by_flags_or_native_description_text() {
    let native = replace(
        &row(at()),
        &[
            ("is_query_store_on", Val::Null),
            ("state", Val::Int(255)),
            ("snapshot_isolation_state", Val::Null),
            ("target_recovery_time_in_seconds", Val::Null),
        ],
    );
    let mut io = FakeRead::new(vec![Ok(vec![native])]);
    let point = read(&mut io).await.unwrap().properties.unwrap();
    assert_eq!(point.query_store_enabled, None);
    assert_eq!(point.target_recovery_time_seconds, None);
    assert_eq!(point.catalog_state.description.as_deref(), Some("ONLINE"));
    assert_eq!(point.state(DatabasePropertyCodeKind::CatalogState), None);
    assert_eq!(point.state(DatabasePropertyCodeKind::SnapshotIsolation), None);
}

#[tokio::test]
async fn hidden_or_unsupported_property_details_retain_the_same_bounded_ordinary_clock_without_fallback_io() {
    for (code, expected) in [(229, ErrorCode::PermissionDenied), (208, ErrorCode::Unsupported)] {
        let fallback = Rec::of(&[
            ("observed_at", Val::Text(at().into())),
            ("metadata_visibility", Val::Int(0)),
            ("selected_database", Val::Text("sample".into())),
            ("property_error_number", Val::Int(code)),
        ]);
        let mut io = FakeRead::new(vec![Ok(vec![fallback])]);
        let frame = read(&mut io).await.unwrap();
        assert_eq!(frame.observed_at, at());
        assert!(!frame.metadata_visible);
        assert_eq!(frame.properties.unwrap_err().code, expected);
        assert_eq!(io.calls.len(), 1);
    }
}

#[tokio::test]
async fn foreign_rows_duplicates_malformed_values_and_terminal_native_errors_do_not_become_properties() {
    for rows in [
        vec![],
        vec![row(at()), row(at())],
        vec![replace(&row(at()), &[("database_id", Val::Int(6))])],
        vec![replace(&row(at()), &[("database_name", Val::Text("foreign".into()))])],
        vec![replace(&row(at()), &[("is_read_only", Val::Int(2))])],
        vec![replace(&row(at()), &[("compatibility_level", Val::Int(-1))])],
        vec![replace(&row(at()), &[("target_recovery_time_in_seconds", Val::Int(-1))])],
    ] {
        let mut io = FakeRead::new(vec![Ok(rows)]);
        let result = read(&mut io).await;
        assert!(result.is_err() || result.unwrap().properties.is_err());
        assert_eq!(io.calls.len(), 1);
    }
    for stop in [
        CollectionStop::TimedOut,
        CollectionStop::Cancelled,
        CollectionStop::CancellationUnconfirmed,
        CollectionStop::ByteLimit,
    ] {
        let mut io = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        assert_eq!(read(&mut io).await.err().unwrap().detail, Some(ErrorDetail::CollectionStopped(stop)));
        assert_eq!(io.calls.len(), 1);
    }
}

#[test]
fn optional_columns_bind_only_inside_fixed_native_sql_and_the_default_authority_clock_is_not_replaced() {
    let sql = query();
    assert!(sql.contains("COL_LENGTH(N'sys.databases',N'is_query_store_on')"));
    assert!(sql.contains("CAST(NULL AS bit) AS is_query_store_on"));
    assert!(sql.contains("sys.sp_executesql @culprit_database_properties"));
    assert!(sql.contains("IF ERROR_NUMBER() NOT IN(207,208,229,297,300) THROW"));
    assert!(!sql.contains("sys.database_query_store_options"), "independent Query Store modes keep their own source");
    assert!(!sql.contains("sys.dm_hadr_database_replica_states"), "catalog state is not a guessed member health read");
}

#[tokio::test]
async fn recorded_checkdb_sentinel_and_null_are_independent_of_utc_acquisition_and_connected_role() {
    for (value, recorded) in [
        (Some("2019-12-28T23:10:00.003"), Some(true)),
        (Some("1900-01-01T00:00:00"), Some(false)),
        (Some("1900-01-01T00:00:00.000"), Some(false)),
        (None, None),
    ] {
        let native = replace(
            &row(at()),
            &[
                ("last_good_checkdb_without_offset", value.map(|value| Val::Text(value.into())).unwrap_or(Val::Null)),
                ("metadata_visibility", Val::Int(i64::from(value.is_some()))),
                ("updateability", Val::Text("READ_ONLY".into())),
            ],
        );
        let mut io = FakeRead::new(vec![Ok(vec![native])]);
        let point = read(&mut io).await.unwrap().properties.unwrap();
        assert_eq!(point.last_good_checkdb_without_offset.as_deref(), value);
        assert_eq!(point.has_recorded_checkdb(), recorded);
        assert_eq!(point.observed_at, at());
        assert_eq!(point.writable(), Some(false), "a reported primary check is not local writeability");
        assert_eq!(io.calls.len(), 1);
        assert!(io.calls[0].sql.contains("DATABASEPROPERTYEX(DB_NAME(),''LastGoodCheckDbTime'')"));
        assert!(!io.calls[0].sql.contains("DBCC "));
    }
}

#[tokio::test]
async fn malformed_or_offset_bearing_checkdb_metadata_cannot_borrow_a_valid_catalog_clock() {
    for value in [
        "2026-11-13T10:00:00Z",
        "2026-11-13T10:00:00+02:00",
        "2026-02-30T10:00:00",
        "2026-11-13T10:00:00.1234",
        "1752-12-31T23:59:59",
        "",
    ] {
        let native = replace(&row(at()), &[("last_good_checkdb_without_offset", Val::Text(value.into()))]);
        let mut io = FakeRead::new(vec![Ok(vec![native])]);
        let result = read(&mut io).await.unwrap();
        assert_eq!(result.observed_at, at());
        assert!(result.properties.is_err(), "{value}");
        assert_eq!(io.calls.len(), 1, "invalid metadata never starts a repair query");
    }
}
