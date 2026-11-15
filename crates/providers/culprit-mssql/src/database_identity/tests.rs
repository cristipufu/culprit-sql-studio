use super::*;
use crate::{
    query_store::window_sql::{
        metadata_sql,
        tests::{azure_row, row as window_row},
    },
    resources::test_support::{FakeRead, replace},
    value::Val,
};

#[test]
fn logical_database_identity_survives_aliases_without_promoting_physical_or_boxed_generations() {
    let context = Context::from_window(&azure_row()).unwrap();
    let primary_alias = context.identity("primary.invalid", 1433);
    let replica_alias = context.identity("readonly.invalid", 1433);
    assert_eq!(primary_alias.telemetry_id().unwrap(), replica_alias.telemetry_id().unwrap());
    let moved = Context::from_window(&replace(
        &azure_row(),
        &[("physical_incarnation", Val::Text("moved-physical-guid".into()))],
    ))
    .unwrap();
    assert_eq!(primary_alias.telemetry_id().unwrap(), moved.identity("primary.invalid", 1433).telemetry_id().unwrap());
    assert_ne!(context, moved, "physical identity still guards this acquisition");
    let recreated =
        Context::from_window(&replace(&azure_row(), &[("incarnation", Val::Text("another-logical-guid".into()))]))
            .unwrap();
    assert_ne!(
        primary_alias.telemetry_id().unwrap(),
        recreated.identity("primary.invalid", 1433).telemetry_id().unwrap()
    );
    let spelled =
        Context::from_window(&replace(&azure_row(), &[("database_name", Val::Text(" Mixed CASE Database ".into()))]))
            .unwrap();
    assert_eq!(spelled.database, " Mixed CASE Database ", "validation never rewrites a native name");

    let boxed = Context::from_window(&window_row()).unwrap();
    assert!(boxed.identity("boxed.invalid", 1433).incarnation.is_none());
    assert_ne!(
        boxed.identity("boxed.invalid", 1433).telemetry_id().unwrap(),
        boxed.identity("another.invalid", 1433).telemetry_id().unwrap()
    );
}

#[test]
fn unknown_or_ambiguous_metadata_never_manufactures_native_identity() {
    let unknown = replace(
        &azure_row(),
        &[("native_identity_rows", Val::Int(0)), ("incarnation", Val::Null), ("physical_incarnation", Val::Null)],
    );
    assert!(Context::from_window(&unknown).unwrap().incarnation.is_none());
    let ambiguous = replace(&azure_row(), &[("native_identity_rows", Val::Int(2))]);
    assert_eq!(
        CollectionStop::from_error(&Context::from_window(&ambiguous).unwrap_err()),
        CollectionStop::SourceChanged
    );
    for changes in [
        vec![("native_identity_rows", Val::Int(-1))],
        vec![("database_name", Val::Text(" \t".into()))],
        vec![("physical_incarnation", Val::Null)],
        vec![("incarnation", Val::Null)],
        vec![("native_identity_rows", Val::Int(0))],
        vec![("incarnation", Val::Text(String::new()))],
        vec![("engine_edition", Val::Int(8))],
    ] {
        assert_eq!(
            CollectionStop::from_error(&Context::from_window(&replace(&azure_row(), &changes)).unwrap_err()),
            CollectionStop::InvalidPage
        );
    }
}

#[test]
fn only_azure_metadata_batches_reference_the_azure_catalog_and_permission_gap_is_narrow() {
    for edition in [2, 3, 4, 8] {
        for verifying in [false, true] {
            let sql = metadata_sql(true, verifying, edition);
            assert!(!sql.contains("sys.dm_user_db_resource_governance"));
            assert!(!sql.contains("logical_database_guid"));
            assert!(!sql.contains("physical_database_guid"));
            assert!(sql.contains("CAST(NULL AS varchar(36)) AS incarnation"));
        }
    }
    for verifying in [false, true] {
        let sql = metadata_sql(true, verifying, 5);
        assert_eq!(sql.matches("sys.dm_user_db_resource_governance").count(), 1);
        assert!(sql.contains("database_id=DB_ID() AND database_name=DB_NAME()"));
        assert!(sql.contains("TOP(2)"));
        assert!(sql.contains(") native OPTION(MAXDOP 1);"));
        assert!(sql.contains("IF ERROR_NUMBER() NOT IN(229,297,300) THROW;"));
        assert!(sql.contains("sys.query_store_runtime_stats_interval"));
        assert!(sql.contains("sys.database_query_store_options"));
    }
}

#[tokio::test]
async fn collector_identity_stays_strict_and_retains_one_existing_native_read() {
    let mut reader = FakeRead::new(vec![Ok(vec![azure_row()])]);
    let context = read_azure(&mut reader).await.unwrap();
    assert_eq!(context.incarnation.as_deref(), Some("logical-database-guid"));
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, 2);
    assert!(reader.calls[0].sql.contains(AZURE_RELATION));
    assert!(reader.calls[0].sql.ends_with("OPTION(MAXDOP 1)"));
    assert_eq!(reader.closed, 0, "the caller still owns completion and shutdown");

    for rows in [
        Vec::new(),
        vec![azure_row(), azure_row()],
        vec![replace(&azure_row(), &[("incarnation", Val::Null), ("physical_incarnation", Val::Null)])],
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read_azure(&mut reader).await.is_err());
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.closed, 0);
    }
}

#[tokio::test]
async fn terminal_and_permission_errors_propagate_without_a_different_identity_query() {
    for failure in [
        ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "fixture metadata denied",
        )),
        ReadError::Stopped(CollectionStop::TimedOut),
        ReadError::Stopped(CollectionStop::CancellationUnconfirmed),
    ] {
        let expected = CollectionStop::from_error(&match &failure {
            ReadError::Source(error) => error.clone(),
            ReadError::Stopped(stop) => stop.error(),
        });
        let mut reader = FakeRead::new(vec![Err(failure)]);
        let error = read_azure(&mut reader).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), expected);
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.closed, 0);
    }
}
