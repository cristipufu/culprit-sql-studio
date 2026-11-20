use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn selection() -> MssqlFullTextPopulationTarget {
    MssqlFullTextPopulationTarget {
        table: MssqlObjectIdentity {
            database_id: 7,
            object_id: 100,
            created_without_offset: "2020-01-01T00:00:00.0000000".into(),
        },
        table_modified_without_offset: "2020-01-01T00:00:00.0000000".into(),
        catalog_id: 2,
        unique_index_id: 1,
    }
}
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("schema_name", Val::Text("dbo".into())),
        ("table_name", Val::Text("OriginalDocuments".into())),
        ("database_id", Val::Int(7)),
        ("object_id", Val::Int(100)),
        ("catalog_id", Val::Int(2)),
        ("unique_index_id", Val::Int(1)),
        ("created_without_offset", Val::Text("2020-01-01T00:00:00.000".into())),
        ("modified_without_offset", Val::Text("2020-01-01T00:00:00.000".into())),
        ("observed_at", Val::Text("2020-01-01T06:00:00.4Z".into())),
        ("can_view_definition", Val::Int(1)),
        ("can_read_state", Val::Int(1)),
        ("layout_available", Val::Int(1)),
        ("population_present", Val::Int(1)),
        ("population_database_id", Val::Int(7)),
        ("population_table_id", Val::Int(100)),
        ("population_catalog_id", Val::Int(2)),
        ("population_type", Val::Int(1)),
        ("population_type_description", Val::Text("Full population".into())),
        ("status", Val::Int(5)),
        ("status_description", Val::Text("Processing normally".into())),
        ("completion_type", Val::Null),
        ("completion_type_description", Val::Null),
        ("queued_population_type", Val::Int(0)),
        ("queued_population_type_description", Val::Null),
        ("is_clustered_index_scan", Val::Bool(true)),
        ("range_count", Val::Int(4)),
        ("completed_range_count", Val::Int(2)),
        ("outstanding_batch_count", Val::Int(0)),
        ("started_without_offset", Val::Text("2020-01-01T00:00:00.007".into())),
    ])
}
pub(crate) fn empty_row() -> Rec {
    let mut fields = RUNTIME_FIELDS.iter().map(|name| (*name, Val::Null)).collect::<Vec<_>>();
    fields.push(("population_present", Val::Int(0)));
    replace(&row(), &fields)
}
fn context() -> NativeReadContext {
    NativeReadContext {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "original.invalid",
            1433,
            None,
        )
        .into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("machine"),
                native: NativeIdentity::new("instance"),
            }),
            epoch: Some("process-and-db".into()),
        },
        observed_from: "2020-01-01T06:00:00Z".into(),
        observed_until: "2020-01-01T06:00:01Z".into(),
        database_writable: Some(true),
    }
}
#[test]
fn sql_checks_selected_native_identity_and_capability_without_user_data_or_population_control_work() {
    let sql = sql::populations(StatePermission::ServerPerformanceState);
    for required in [
        "TOP(@P1)",
        "p.database_id=DB_ID()",
        "p.table_id=t.object_id",
        "p.catalog_id=ft.fulltext_catalog_id",
        "t.object_id=@P2",
        "ft.fulltext_catalog_id=@P3",
        "ft.unique_index_id=@P4",
        "VIEW SERVER PERFORMANCE STATE",
        "IF (@permission=1 OR 0=1) AND @layout=1",
    ] {
        assert!(sql.contains(required), "{required}");
    }
    for forbidden in [
        "OBJECTPROPERTY(",
        "TableFulltextPendingChanges",
        "dm_fts_population_ranges",
        "memory_address",
        "worker_count",
        "SELECT *",
        "ORDER BY",
        "ALTER FULLTEXT",
        "START",
        "STOP",
    ] {
        assert!(!sql.contains(forbidden), "{forbidden}");
    }
    assert!(sql.ends_with("END;"));
    assert!(!sql.ends_with("END; OPTION(MAXDOP 1)"));
}
#[tokio::test]
async fn original_native_scope_nulls_zero_and_unknown_status_are_retained_without_extra_queries() {
    let selected = selection();
    let context = context();
    for status in [Some(5), Some(7), Some(11), Some(255), Some(-1), None] {
        let native = replace(&row(), &[("status", status.map(Val::Int).unwrap_or(Val::Null))]);
        let mut reader = FakeRead::new(vec![Ok(vec![native])]);
        let read = read(&mut reader, &selected, 4, StatePermission::ServerPerformanceState).await.unwrap();
        assert_eq!(read.coverage.state, culprit_core::SourceState::Collected);
        let records = read.records(&context, &selected).unwrap();
        let TelemetryRecord::Operational(row) = &records[0] else { panic!() };
        let OperationalFact::MssqlFullTextPopulation(point) = &row.fact else { panic!() };
        assert_eq!(point.status, status.map(|value| value as i32));
        assert_eq!(point.outstanding_batch_count, Some(0));
        assert_eq!(point.started_without_offset.as_deref(), Some("2020-01-01T00:00:00.007"));
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.calls[0].parameters.len(), 4);
        assert_eq!(reader.calls[0].limit, 5);
    }
}
#[tokio::test]
async fn permission_layout_unknown_and_empty_keep_distinct_availability_without_a_second_query() {
    for (permission, layout, expected) in [
        (Some(0), Some(1), Some(ErrorCode::PermissionDenied)),
        (Some(1), Some(0), Some(ErrorCode::Unsupported)),
        (None, Some(1), Some(CollectionStop::SourceUnavailable.error().code)),
        (Some(1), Some(1), None),
    ] {
        let native = replace(
            &empty_row(),
            &[
                ("can_read_state", permission.map(Val::Int).unwrap_or(Val::Null)),
                ("layout_available", layout.map(Val::Int).unwrap_or(Val::Null)),
                ("population_present", Val::Int(0)),
            ],
        );
        let mut reader = FakeRead::new(vec![Ok(vec![native])]);
        let result = read(&mut reader, &selection(), 4, StatePermission::ServerPerformanceState).await.unwrap();
        assert_eq!(result.coverage.error_code, expected);
        assert!(result.records(&context(), &selection()).unwrap().is_empty());
        assert_eq!(reader.calls.len(), 1);
    }
}
#[tokio::test]
async fn changed_actual_native_database_catalog_table_index_or_generation_refuses_the_rows() {
    for field in [
        "database_id",
        "object_id",
        "catalog_id",
        "unique_index_id",
        "population_database_id",
        "population_table_id",
        "population_catalog_id",
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[(field, Val::Int(999))])])]);
        assert!(read(&mut reader, &selection(), 4, StatePermission::ServerPerformanceState).await.is_err(), "{field}");
    }
    for field in ["created_without_offset", "modified_without_offset"] {
        let mut reader =
            FakeRead::new(vec![Ok(vec![replace(&row(), &[(field, Val::Text("2020-01-01T00:00:00.0000001".into()))])])]);
        assert!(read(&mut reader, &selection(), 4, StatePermission::ServerPerformanceState).await.is_err(), "{field}");
    }
}
#[tokio::test]
async fn malformed_counts_time_or_native_description_fail_binding_instead_of_becoming_zero_or_utc() {
    for (field, value) in [
        ("range_count", Val::Int(-1)),
        ("status", Val::Text("5".into())),
        ("started_without_offset", Val::Text("2020-01-01T00:00:00Z".into())),
        ("status_description", Val::Text("hidden\n\u{1b}[31m".into())),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[(field, value)])])]);
        let native = read(&mut reader, &selection(), 4, StatePermission::ServerPerformanceState).await.unwrap();
        assert!(native.records(&context(), &selection()).is_err(), "{field}");
    }
}

#[test]
fn documented_native_permissions_select_one_query_without_a_guessed_azure_tier_or_retry() {
    let legacy = sql::populations(StatePermission::ServerState);
    assert!(legacy.contains("HAS_PERMS_BY_NAME(NULL,NULL,N'VIEW SERVER STATE')"));
    assert!(!legacy.contains("VIEW SERVER PERFORMANCE STATE"));
    let current = sql::populations(StatePermission::ServerPerformanceState);
    assert!(current.contains("HAS_PERMS_BY_NAME(NULL,NULL,N'VIEW SERVER PERFORMANCE STATE')"));
    let azure = sql::populations(StatePermission::AzureDatabase);
    assert!(azure.contains("@permission int=NULL"));
    assert!(azure.contains("OR 1=1) AND @layout=1"));
    for guessed in ["ServiceObjective", "VIEW DATABASE STATE", "IS_SRVROLEMEMBER", "WAITFOR", "RETRY"] {
        assert!(!azure.contains(guessed));
    }
}

#[tokio::test]
async fn contradictory_nonnull_runtime_values_cannot_be_admitted_as_an_empty_or_denied_population() {
    for permission in [0, 1] {
        let row = replace(&row(), &[("population_present", Val::Int(0)), ("can_read_state", Val::Int(permission))]);
        let mut reader = FakeRead::new(vec![Ok(vec![row])]);
        assert!(read(&mut reader, &selection(), 4, StatePermission::ServerPerformanceState).await.is_err());
    }
}
