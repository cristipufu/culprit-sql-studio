use super::test_support::*;
use super::*;
use crate::resources::test_support::{FakeRead, replace, window};
use crate::value::Val;

#[tokio::test]
async fn member_routes_share_a_native_pool_identity_without_claiming_a_database_or_incarnation() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![identity_row()]),
        Ok(vec![replace(
            &identity_row(),
            &[
                ("incarnation", Val::Text("another-logical-member".into())),
                ("physical_incarnation", Val::Text("another-physical-member".into())),
            ],
        )]),
    ]);
    let (first, _) = identity(&mut reader).await.unwrap();
    let (second, _) = identity(&mut reader).await.unwrap();
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!(first.identity.telemetry_id().unwrap(), second.identity.telemetry_id().unwrap());
    assert!(first != second);
    assert_eq!(first.identity.kind, SharedResourceKind::Pool);
    assert_eq!(first.identity.namespace.id, "fixture-server");
    assert_eq!(first.identity.native.id, "fixture-pool");
    assert!(reader.calls.iter().all(|call| call.limit == 2 && call.sql.contains("/* culprit */")));
    assert!(!reader.calls[0].sql.contains("JOIN sys.database_service_objectives"));
}

#[tokio::test]
async fn a_nonpooled_database_is_absent_and_invalid_or_secondary_metadata_is_never_guessed() {
    let mut reader = FakeRead::new(vec![Ok(vec![replace(&identity_row(), &[("pool_name", Val::Null)])])]);
    assert!(identity(&mut reader).await.unwrap().0.is_none());
    for rows in [vec![], vec![identity_row(), identity_row()]] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(
            identity(&mut reader).await.is_err(),
            "hidden or ambiguous membership is not proof of an unpooled database"
        );
    }
    for changes in [
        vec![("pool_name", Val::Text("".into()))],
        vec![("server_name", Val::Null)],
        vec![("updateability", Val::Text("READ_ONLY".into()))],
        vec![("cpu_limit", Val::Float(f64::NAN))],
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&identity_row(), &changes)])]);
        assert!(identity(&mut reader).await.is_err());
    }
}

#[test]
fn pool_fields_preserve_unknown_values_shared_scope_and_native_aggregation_limits() {
    let record = sample::decode(&row("2020-01-01T06:00:15Z"), Pricing::Vcore).unwrap();
    let metric = |key: &str| record.metrics.iter().find(|metric| metric.key == key).unwrap();
    assert_eq!(record.replica_id, None);
    assert_eq!(metric("cpu").value, Some(0.0));
    assert_eq!(metric("data_io").value, None);
    assert_eq!(metric("edtu").value, None);
    assert_eq!(metric("used_storage").value, Some(512.0 * 1024.0 * 1024.0));
    assert_eq!(metric("used_storage").unit, ResourceUnit::Bytes);
    assert_eq!(metric("workers").statistic, ResourceStatistic::Maximum);
    assert!(record.metrics.iter().all(|metric| metric.period_ms.is_none()));
    let pool = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Pool,
        namespace: NativeIdentity::new("fixture-server"),
        native: NativeIdentity::new("fixture-pool"),
    };
    let target = CollectionTarget::Shared(pool);
    assert!(metric("cpu").describes_subject(&target));
    assert!(!metric("instance_memory").describes_subject(&target));
    let dtu = sample::decode(&row("2020-01-01T06:00:15Z"), Pricing::Dtu).unwrap();
    assert_eq!(dtu.metrics.iter().find(|metric| metric.key == "edtu").unwrap().value, Some(0.0));
    for value in [Val::Float(-1.0), Val::Float(f64::INFINITY), Val::Text("unknown".into())] {
        assert!(
            sample::decode(&replace(&row("2020-01-01T06:00:15Z"), &[("avg_cpu_percent", value)]), Pricing::Vcore)
                .is_err()
        );
    }
}

#[tokio::test]
async fn pool_history_reuses_bound_selection_and_validates_the_overflow_witness() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![identity_row()]),
        Ok(vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:30Z")]),
    ]);
    let context = identity(&mut reader).await.unwrap().0.unwrap();
    assert_eq!(windowed(&mut reader, &context, &window(), None, 1).await.unwrap().len(), 2);
    let call = &reader.calls[1];
    assert_eq!(call.limit, 2);
    assert!(call.sql.contains("sys.dm_elastic_pool_resource_stats"));
    assert!(!call.sql.contains("avg_storage_percent"));
    assert!(!call.sql.contains("replica_role"));
    assert!(!call.sql.contains("2020-01-01"));
    for invalid in [vec![row("2020-01-01T06:00:15Z"), row("2020-01-01T06:00:15Z")], vec![row("2020-01-01T06:01:00Z")]] {
        let mut reader = FakeRead::new(vec![Ok(invalid)]);
        assert!(windowed(&mut reader, &context, &window(), None, 1).await.is_err());
    }
}
