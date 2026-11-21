use super::super::test_support::{settings_layout, settings_member, settings_point};
use super::*;
use crate::{
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
const BEFORE: &str = "2020-01-01T06:00:00Z";
const POINT: &str = "2020-01-01T06:00:02Z";
async fn owner() -> Context {
    let mut reader = FakeRead::new(vec![Ok(vec![settings_member(BEFORE)])]);
    let owner = Context::read(&mut reader, &CollectionSubject::Database, "fixture.invalid", 1433).await.unwrap();
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    owner
}
fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    vec![Ok(settings_layout()), Ok(vec![settings_point(POINT)])]
}
#[tokio::test]
async fn selected_native_singleton_keeps_pool_units_nullable_zero_and_configuration_without_database_limits() {
    let owner = owner().await;
    let mut reader = FakeRead::new(replies());
    let read = read(&mut reader, &owner, 6).await.unwrap();
    assert_eq!(read.values.len(), 6);
    assert_eq!(read.point.database_id, 7);
    assert_eq!(read.point.observed_at, "2020-01-01T06:00:02.000000000Z");
    assert_eq!(read.values[0].value.as_deref(), Some("0"));
    assert_eq!(read.values[3].value, None);
    assert_eq!(read.values[5].unit.as_deref(), Some("io-operations-per-second"));
    assert!(read.values.iter().all(|row| row.scope == MeasurementScope::Pool
        && row.nature == SettingNature::Configuration
        && row.value_in_use.is_none()
        && row.pending_restart.is_none()));
    assert_eq!(reader.queries(), 2);
    assert_eq!(reader.calls[0].limit, 7);
    assert_eq!(reader.calls[1].limit, 2);
    assert!(reader.calls[1].sql.contains("g.database_id=DB_ID() AND g.database_name=DB_NAME()"));
    assert!(!reader.calls[1].sql.contains("dm_resource_governor_resource_pools"));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn missing_duplicate_or_incompatible_native_columns_refuse_the_projection_without_dynamic_fallback() {
    let owner = owner().await;
    for case in ["missing", "duplicate", "type", "name"] {
        let mut rows = settings_layout();
        match case {
            "missing" => {
                rows.pop();
            }
            "duplicate" => rows[5] = rows[0].clone(),
            "type" => rows[0] = replace(&rows[0], &[("system_type_id", Val::Int(62))]),
            _ => rows[0] = replace(&rows[0], &[("name", Val::Text("cpu_limit".into()))]),
        }
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert_eq!(read(&mut reader, &owner, 6).await.err().unwrap().code, ErrorCode::Unsupported, "{case}");
        assert_eq!(reader.queries(), 1);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn selected_member_role_database_and_native_clock_changes_refuse_without_substituting_pool_names_or_ids() {
    let owner = owner().await;
    for (name, value) in [
        ("pool_name", Val::Text("foreign-pool".into())),
        ("server_name", Val::Text("foreign-server".into())),
        ("incarnation", Val::Text("33333333-3333-3333-3333-333333333333".into())),
        ("physical_incarnation", Val::Text("44444444-4444-4444-4444-444444444444".into())),
        ("selected_database_id", Val::Int(8)),
        ("database_id", Val::Int(0)),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("observed_at", Val::Text("2019-01-01T00:00:00Z".into())),
    ] {
        let mut reader =
            FakeRead::new(vec![Ok(settings_layout()), Ok(vec![replace(&settings_point(POINT), &[(name, value)])])]);
        assert!(read(&mut reader, &owner, 6).await.is_err(), "{name}");
        assert_eq!(reader.queries(), 2);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn native_permissions_and_unconfirmed_stops_remain_errors_without_retries_or_new_queries() {
    let owner = owner().await;
    for result in [
        ReadError::Source(Error::new(ErrorCode::PermissionDenied, "owned denial")),
        ReadError::Stopped(CollectionStop::CancellationUnconfirmed),
        ReadError::Stopped(CollectionStop::Cancelled),
        ReadError::Stopped(CollectionStop::TimedOut),
    ] {
        let mut reader = FakeRead::new(vec![Err(result)]);
        assert!(read(&mut reader, &owner, 6).await.is_err());
        assert_eq!(reader.queries(), 1);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
    let mut reader = FakeRead::new(vec![]);
    assert_eq!(
        CollectionStop::from_error(&read(&mut reader, &owner, 5).await.err().unwrap()),
        CollectionStop::RowLimit
    );
    assert_eq!(reader.queries(), 0);
}
#[tokio::test]
async fn native_wrong_numeric_types_negative_values_and_nonfinite_values_never_become_nullable_or_zero() {
    let owner = owner().await;
    for value in [Val::Int(-1), Val::Int(101), Val::Float(1.0), Val::Float(f64::NAN), Val::Text("0".into())] {
        let mut reader = FakeRead::new(vec![
            Ok(settings_layout()),
            Ok(vec![replace(&settings_point(POINT), &[("cap_cpu", value)])]),
        ]);
        assert!(read(&mut reader, &owner, 6).await.is_err());
        assert_eq!(reader.queries(), 2);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[test]
fn optional_catalog_is_bounded_dynamically_guarded_and_never_referenced_by_the_final_settings_select() {
    let sql = query(COLUMNS);
    assert!(sql.contains("IF OBJECT_ID(N'sys.database_service_objectives') IS NOT NULL"));
    assert!(sql.contains("EXEC sys.sp_executesql"));
    assert!(sql.contains("SELECT /* culprit */ @rows=COUNT(*)"));
    assert!(sql.contains("SELECT TOP(2) s.elastic_pool_name"));
    assert!(sql.contains("IF @culprit_membership_rows=1 SET @culprit_membership_visibility=0"));
    assert!(sql.contains("IF @culprit_membership_visibility<>0 SET @culprit_pool_name=NULL"));
    let final_select = sql.rsplit("SELECT /* culprit */ g.server_name").next().unwrap();
    assert!(!final_select.contains("sys.database_service_objectives"));
    assert!(!final_select.contains("CROSS JOIN"));
    assert!(final_select.contains("g.database_id=DB_ID() AND g.database_name=DB_NAME()"));
    for value in [Val::Null, Val::Int(-1), Val::Int(3), Val::Text("0".into())] {
        assert!(membership_error(&replace(&settings_member(BEFORE), &[("elastic_pool_visibility", value)])).is_err());
    }
}
