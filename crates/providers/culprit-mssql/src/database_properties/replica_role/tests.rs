use super::super::tests::row;
use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

const AT: &str = "2026-11-18T12:00:00.0000001Z";
const BEFORE: &str = "2026-11-18T12:00:00Z";
const REPLICA: &str = "91487244-730d-4b83-8e49-d482241c7288";
const GROUP_DATABASE: &str = "71bd65c3-bd16-4d08-b5ae-d72e3bf2f6d4";

fn local_row(primary: Val) -> Rec {
    replace(
        &row(AT),
        &[
            ("local_role_state", Val::Int(LOCAL_AVAILABILITY_GROUP)),
            ("local_role_interface", Val::Int(HADR_INTERFACE)),
            ("local_role_rows", Val::Int(1)),
            ("local_role_replica_id", Val::Text(REPLICA.into())),
            ("local_role_group_database_id", Val::Text(GROUP_DATABASE.into())),
            ("local_role_is_primary", primary),
        ],
    )
}

#[tokio::test]
async fn azure_catalog_and_current_ids_remain_independent_and_only_the_original_native_name_selects_the_row() {
    let native = replace(
        &row(AT),
        &[
            ("native_engine_edition", Val::Int(5)),
            ("selected_database_id", Val::Int(7)),
            ("catalog_database_id", Val::Int(42)),
            ("local_role_catalog_id", Val::Int(42)),
            ("local_role_state", Val::Int(UNQUALIFIED_OBSERVER)),
        ],
    );
    let mut io = FakeRead::new(vec![Ok(vec![native])]);
    let point = super::super::read(&mut io).await.unwrap().properties.unwrap();
    assert_eq!(point.native_database_id, "7");
    assert_eq!(point.catalog_database_id, "42");
    assert_eq!(point.catalog_created_without_offset.as_deref(), Some("2019-12-01T08:30:00.003"));
    assert_eq!(io.calls.len(), 1);
    assert_eq!(io.calls[0].limit, 2);
    assert!(io.calls[0].parameters.is_empty());
    assert!(matches!(
        point.local_replica_role,
        Some(MssqlDatabaseReplicaRole::Unavailable {
            reason: MssqlDatabaseReplicaRoleUnavailable::UnqualifiedObserver,
            ..
        })
    ));
    for changes in [vec![("database_name", Val::Text("foreign".into()))], vec![("catalog_database_id", Val::Int(6))]] {
        let mut io = FakeRead::new(vec![Ok(vec![replace(&row(AT), &changes)])]);
        assert_eq!(
            super::super::read(&mut io).await.unwrap().properties.unwrap_err().detail,
            CollectionStop::SourceChanged.error().detail
        );
        assert_eq!(io.calls.len(), 1, "foreign metadata does not cause a retry");
    }
}

#[tokio::test]
async fn native_catalog_local_time_and_the_earlier_role_utc_point_preserve_the_whole_original_interval() {
    let native = replace(&row(AT), &[("local_role_observed_at", Val::Text(BEFORE.into()))]);
    let mut io = FakeRead::new(vec![Ok(vec![native])]);
    let read = super::super::read(&mut io).await.unwrap();
    let point = read.properties.unwrap();
    assert_eq!(read.observed_at, BEFORE, "source start includes the already-observed role point");
    assert_eq!(point.observed_at, AT, "catalog point retains its own original clock");
    assert_eq!(point.observed_from(), BEFORE);
    for value in [Val::Null, Val::Text("2019-12-01T08:30:00.003".into())] {
        let mut io =
            FakeRead::new(vec![Ok(vec![replace(&row(AT), &[("catalog_created_without_offset", value.clone())])])]);
        let point = super::super::read(&mut io).await.unwrap().properties.unwrap();
        assert_eq!(
            point.catalog_created_without_offset,
            match value {
                Val::Text(value) => Some(value),
                _ => None,
            }
        );
    }
    for value in
        ["2019-12-01T08:30:00Z", "2019-12-01T08:30:00+02:00", "2019-02-30T08:30:00", "2019-12-01T08:30:00.1234"]
    {
        let mut io = FakeRead::new(vec![Ok(vec![replace(
            &row(AT),
            &[("catalog_created_without_offset", Val::Text(value.into()))],
        )])]);
        assert!(super::super::read(&mut io).await.unwrap().properties.is_err(), "{value}");
    }
}

#[tokio::test]
async fn exact_native_local_primary_secondary_and_null_do_not_borrow_updateability_or_a_geo_role() {
    for (native, expected) in [(Val::Bool(true), Some(true)), (Val::Bool(false), Some(false)), (Val::Null, None)] {
        let native = replace(&local_row(native), &[("updateability", Val::Text("READ_ONLY".into()))]);
        let mut io = FakeRead::new(vec![Ok(vec![native])]);
        let point = super::super::read(&mut io).await.unwrap().properties.unwrap();
        assert_eq!(point.writable(), Some(false));
        assert!(matches!(point.local_replica_role, Some(MssqlDatabaseReplicaRole::LocalAvailabilityGroup {
            source: MssqlDatabaseReplicaRoleSource::HadrDatabaseReplicaStates,
            is_primary, ..
        }) if is_primary == expected));
        assert_eq!(io.calls.len(), 1);
    }
    let native = replace(
        &local_row(Val::Bool(true)),
        &[
            ("native_engine_edition", Val::Int(5)),
            ("catalog_database_id", Val::Int(42)),
            ("local_role_catalog_id", Val::Int(42)),
            ("local_role_interface", Val::Int(AZURE_INTERFACE)),
            ("local_role_observer_replica_id", Val::Text(REPLICA.to_uppercase())),
        ],
    );
    let mut io = FakeRead::new(vec![Ok(vec![native])]);
    let point = super::super::read(&mut io).await.unwrap().properties.unwrap();
    assert!(matches!(
        point.local_replica_role,
        Some(MssqlDatabaseReplicaRole::LocalAvailabilityGroup {
            source: MssqlDatabaseReplicaRoleSource::DatabaseReplicaStates,
            is_primary: Some(true),
            ..
        })
    ));
}

#[tokio::test]
async fn optional_role_layout_denial_empty_ambiguity_and_failure_preserve_the_same_catalog_properties() {
    for (state, reason, error) in [
        (UNSUPPORTED_PLATFORM, MssqlDatabaseReplicaRoleUnavailable::UnsupportedPlatform, None),
        (UNSUPPORTED_LAYOUT, MssqlDatabaseReplicaRoleUnavailable::UnsupportedLayout, Some(208)),
        (PERMISSION_DENIED, MssqlDatabaseReplicaRoleUnavailable::PermissionDenied, Some(229)),
        (VISIBILITY_UNAVAILABLE, MssqlDatabaseReplicaRoleUnavailable::VisibilityUnavailable, None),
        (METADATA_UNAVAILABLE, MssqlDatabaseReplicaRoleUnavailable::MetadataUnavailable, None),
        (UNQUALIFIED_OBSERVER, MssqlDatabaseReplicaRoleUnavailable::UnqualifiedObserver, None),
        (MISSING_LOCAL_ROW, MssqlDatabaseReplicaRoleUnavailable::MissingLocalRow, None),
        (AMBIGUOUS_LOCAL_ROWS, MssqlDatabaseReplicaRoleUnavailable::AmbiguousLocalRows, None),
        (NATIVE_READ_FAILED, MssqlDatabaseReplicaRoleUnavailable::NativeReadFailed, Some(1222)),
    ] {
        let native = replace(
            &row(AT),
            &[
                ("local_role_state", Val::Int(state)),
                ("local_role_error_number", error.map(Val::Int).unwrap_or(Val::Null)),
            ],
        );
        let mut io = FakeRead::new(vec![Ok(vec![native])]);
        let point = super::super::read(&mut io).await.unwrap().properties.unwrap();
        assert_eq!(point.catalog_state.code, Some(0));
        assert_eq!(point.compatibility_level, Some(160));
        assert!(matches!(point.local_replica_role, Some(MssqlDatabaseReplicaRole::Unavailable {
            reason: actual, native_error_number, ..
        }) if actual == reason && native_error_number == error.map(|value|value as u32)));
        assert_eq!(io.calls.len(), 1);
    }
}

#[tokio::test]
async fn no_group_requires_visible_online_boxed_or_mi_membership_nulls_and_local_rows_require_exact_scope() {
    for edition in [2, 3, 4, 8] {
        let mut io = FakeRead::new(vec![Ok(vec![replace(&row(AT), &[("native_engine_edition", Val::Int(edition))])])]);
        assert!(matches!(
            super::super::read(&mut io).await.unwrap().properties.unwrap().local_replica_role,
            Some(MssqlDatabaseReplicaRole::NotInAvailabilityGroup { .. })
        ));
    }
    for changes in [
        vec![("native_engine_edition", Val::Int(5))],
        vec![("local_role_metadata_visibility", Val::Null)],
        vec![("local_role_metadata_visibility", Val::Int(0))],
        vec![("local_role_catalog_state", Val::Int(1))],
        vec![("local_role_replica_id", Val::Text(REPLICA.into()))],
        vec![("local_role_group_database_id", Val::Text(GROUP_DATABASE.into()))],
        vec![("local_role_catalog_id", Val::Int(6))],
    ] {
        let mut io = FakeRead::new(vec![Ok(vec![replace(&row(AT), &changes)])]);
        assert!(super::super::read(&mut io).await.unwrap().properties.is_err(), "{changes:?}");
    }
    for changes in [
        vec![("local_role_rows", Val::Int(0))],
        vec![("local_role_rows", Val::Int(2))],
        vec![("local_role_interface", Val::Int(99))],
        vec![("native_engine_edition", Val::Int(5))],
        vec![("local_role_error_number", Val::Int(229))],
        vec![("local_role_is_primary", Val::Int(2))],
        vec![("local_role_replica_id", Val::Text("00000000-0000-0000-0000-000000000000".into()))],
        vec![("local_role_observed_at", Val::Text("2026-11-18T12:00:01Z".into()))],
    ] {
        let mut io = FakeRead::new(vec![Ok(vec![replace(&local_row(Val::Bool(true)), &changes)])]);
        assert!(super::super::read(&mut io).await.unwrap().properties.is_err(), "{changes:?}");
    }
}

#[test]
fn one_result_role_batch_binds_only_native_local_metadata_and_does_not_change_reader_population() {
    let sql = super::super::query();
    assert!(
        sql.contains("@culprit_properties_database_id int=DB_ID(),@culprit_properties_database_name sysname=DB_NAME()")
    );
    assert!(sql.contains("d.database_id AS catalog_database_id"));
    assert!(sql.contains("d.name=@culprit_properties_database_name"));
    assert!(!sql.contains("d.database_id=DB_ID()"));
    for interface in ["sys.dm_hadr_database_replica_states", "sys.dm_database_replica_states"] {
        let role = native_role_sql(interface);
        assert!(role.contains("SELECT TOP(2)"));
        assert!(role.contains("database_id=@database_id AND is_local=1 AND replica_id=@replica_id AND group_database_id=@group_database_id"));
        assert!(
            role.starts_with("SELECT /* culprit */ @rows="),
            "native inner lookup has only scalar output parameters"
        );
    }
    assert!(sql.contains("@rows=@culprit_role_rows OUTPUT,@is_primary=@culprit_role_is_primary OUTPUT"));
    assert!(sql.contains("VIEW SERVER PERFORMANCE STATE"));
    assert!(sql.contains("VIEW DATABASE STATE"));
    for forbidden in [
        "#",
        "DECLARE @rows TABLE",
        "COALESCE(",
        "TOP(1)",
        "sys.fn_hadr_is_primary_replica",
        "sys.tables",
        "sys.dm_geo_replication_link_status",
        "synchronization_health",
    ] {
        assert!(!sql.contains(forbidden), "{forbidden}");
    }
    assert_eq!(sql.matches("sys.sp_executesql @culprit_database_properties").count(), 1);
}

#[test]
fn membership_assignment_ends_before_the_native_from_clause_in_the_actual_batch() {
    let boundary = "@group_database_id=d.group_database_id FROM sys.databases";
    assert!(MEMBERSHIP.contains(boundary));
    assert!(batch().contains(boundary));
    assert!(!super::super::query().contains("d.group_database_idFROM"));
}
